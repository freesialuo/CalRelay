//! Local-only maintenance. No keys, network, notification outputs, or serving process.
use calendar_storage::backup;
use std::path::Path;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["backup", source, destination] => {
            backup::backup(Path::new(source), Path::new(destination)).await
        }
        ["restore", source, destination, cutoff] => match cutoff.parse::<i64>() {
            Ok(cutoff) => backup::restore(Path::new(source), Path::new(destination), cutoff).await,
            Err(_) => {
                eprintln!("cutoff must be UTC Unix milliseconds");
                return std::process::ExitCode::FAILURE;
            }
        },
        ["resume", database] => backup::resume(Path::new(database)).await,
        _ => {
            eprintln!("usage: calrelay-maintenance backup SOURCE NEW_DIR | restore BACKUP_DIR NEW_DIR CUTOFF_MS | resume DATABASE");
            return std::process::ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => {
            println!("maintenance completed");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
