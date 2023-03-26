use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Client;
use scraper::error::SelectorErrorKind;
use scraper::{Html, Selector};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use thiserror::Error;
use tokio::io::{
  AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter, Lines,
};
use tokio::process::Command;
use tokio::sync::mpsc::{self, Sender};
use tokio::task::{JoinError, JoinHandle};

#[derive(Debug, Error)]
pub enum RunMinecraftError {
  #[error("error in reading Minecraft world directory")]
  IoError(#[from] std::io::Error),
  #[error("generic error in processing Minecraft world")]
  GenericError(#[from] Box<dyn std::error::Error>),
  #[error("target directory does not contain a Minecraft world")]
  NoWorld,
  #[error("error in fetching Minecraft webpage")]
  CouldNotFetch(#[from] reqwest::Error),
  #[error("error in decoding Minecraft webpage")]
  BadSelector(#[from] SelectorErrorKind<'static>),
  #[error("error finding latest Minecraft server")]
  CouldNotFindServer,
  #[error("threading error")]
  JoinError(#[from] JoinError),
  #[error("threading error")]
  ThreadError(#[from] ThreadError),
}

#[derive(Debug, Error)]
pub enum ThreadError {
  #[error("error in reading Minecraft output")]
  IoError(#[from] std::io::Error),
  #[error("error in forwarding Minecraft output")]
  SendError(#[from] tokio::sync::mpsc::error::SendError<OutputMessage>),
}

fn path_exists(path: &PathBuf) -> bool {
  path.is_dir() || path.is_file()
}

fn is_likely_minecraft_directory(path: &PathBuf) -> bool {
  let world_dir = path.join("world");
  path_exists(&world_dir) || path_exists(&world_dir.join("level.dat"))
}

#[derive(Debug)]
pub struct OutputMessage(String);

fn run_grab_output_thread(
  mut reader: Lines<BufReader<impl 'static + AsyncRead + Send + Unpin>>,
  sender: Sender<OutputMessage>,
) -> JoinHandle<Result<(), ThreadError>> {
  tokio::spawn(async move {
    while let Some(line) = reader.next_line().await? {
      sender.send(OutputMessage(line)).await?;
    }

    Ok(())
  })
}

#[tokio::main]
pub async fn run_minecraft_server(
  path: &PathBuf,
  output_sink: impl AsyncWrite + Unpin,
) -> Result<(), RunMinecraftError> {
  if !is_likely_minecraft_directory(path) {
    return Err(RunMinecraftError::NoWorld);
  }

  let mut headers = HeaderMap::new();
  headers.insert("Accept-Encoding", HeaderValue::from_static("gzip"));
  headers.insert(
    "User-Agent",
    HeaderValue::from_str(
      format!("Minecraft Tools v{}", env!("CARGO_PKG_VERSION"))
        .to_string()
        .as_str(),
    )
    .expect("Could not create user-agent header"),
  );
  let client = Client::builder().default_headers(headers).build()?;
  let webpage_html = client
    .get("https://www.minecraft.net/en-us/download/server")
    .send()
    .await?
    .text()
    .await?;
  let document = Html::parse_document(&webpage_html);
  let selector = Selector::parse("a[aria-label='mincraft version']")?;
  let link = document
    .select(&selector)
    .next()
    .ok_or(RunMinecraftError::CouldNotFindServer)?;
  let server_filename = link.inner_html();
  let server_path = path.join(&server_filename);
  let server_download_url = link
    .value()
    .attr("href")
    .ok_or(RunMinecraftError::CouldNotFindServer)?;

  if !path_exists(&server_path) {
    let mut file = File::create(&server_path)?;
    file.write(
      &client
        .get(server_download_url)
        .send()
        .await?
        .bytes()
        .await?,
    )?;
  }

  let eula_file = path.join("eula.txt");
  if !path_exists(&eula_file) {
    fs::write(eula_file, "eula=true")?;
  }

  let mut minecraft_server = Command::new("java")
    .current_dir(path)
    .args(["-Xmx1024M", "-Xms1024M", "-jar", &server_filename, "nogui"])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
  let stdout = BufReader::new(
    minecraft_server
      .stdout
      .take()
      .expect("Could not get Minecraft server stdout"),
  )
  .lines();
  let stderr = BufReader::new(
    minecraft_server
      .stderr
      .take()
      .expect("Could not get Minecraft server stderr"),
  )
  .lines();

  let mut output_sink = BufWriter::new(output_sink);
  let (sender, mut receiver) = mpsc::channel(1);
  let threads = [
    run_grab_output_thread(stdout, sender.clone()),
    run_grab_output_thread(stderr, sender),
  ];
  while let Some(OutputMessage(message)) = receiver.recv().await {
    output_sink.write_all(message.as_bytes()).await?;
    output_sink.write_u8(b'\n').await?;
    output_sink.flush().await?;
  }
  for thread in threads {
    thread.await??;
  }

  Ok(())
}
