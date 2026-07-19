use std::path::PathBuf;
use std::time::Duration;

use imbh::Db;
use imbh_tui::{Options, parse_datetime, run};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let Some(path) = arguments.next() else {
        return Err(
            "usage: imbh-tui <database-directory> [--ascii] [--refresh-seconds N] \
             [--from 'YYYY-MM-DD HH:MM:SS' --to 'YYYY-MM-DD HH:MM:SS']"
                .into(),
        );
    };
    let mut options = Options::default();
    let (mut from, mut to) = (None, None);
    while let Some(argument) = arguments.next() {
        if argument == "--ascii" {
            options.ascii = true;
        } else if argument == "--refresh-seconds" {
            let value = arguments
                .next()
                .ok_or("--refresh-seconds requires an integer")?;
            let seconds = value
                .to_string_lossy()
                .parse::<u64>()
                .map_err(|_| "--refresh-seconds requires an integer")?;
            options.refresh_interval = Duration::from_secs(seconds.max(1));
        } else if argument == "--from" {
            let value = arguments.next().ok_or("--from requires a UTC datetime")?;
            from = Some(
                parse_datetime(&value.to_string_lossy())
                    .ok_or("--from: expected UTC 'YYYY-MM-DD HH:MM:SS'")?,
            );
        } else if argument == "--to" {
            let value = arguments.next().ok_or("--to requires a UTC datetime")?;
            to = Some(
                parse_datetime(&value.to_string_lossy())
                    .ok_or("--to: expected UTC 'YYYY-MM-DD HH:MM:SS'")?,
            );
        } else {
            return Err(format!("unknown argument: {}", argument.to_string_lossy()).into());
        }
    }
    // An absolute launch window needs both bounds, ordered.
    options.window = match (from, to) {
        (Some(start), Some(end)) if start < end => Some((start, end)),
        (Some(_), Some(_)) => return Err("--from must be before --to".into()),
        (None, None) => None,
        _ => return Err("--from and --to must be given together".into()),
    };
    let db = Db::open_read_only(PathBuf::from(path))?;
    run(db, options).await?;
    Ok(())
}
