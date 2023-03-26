use clap::Parser;
use std::path::PathBuf;
use tokio::io::stdout;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
  /// Directory containing the minecraft server
  directory: PathBuf,
}

fn main() {
  let directory = Args::parse().directory;

  println!("Running Minecraft from \"{}\"...", directory.display());
  if let Err(error) = run::run_minecraft_server(&directory, stdout()) {
    eprintln!("Error: {error}");
  }
}
