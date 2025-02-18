use chrono::{DateTime, Utc};
use config::Config;
use data::Data;
use reqwest::{Client, StatusCode};
use std::{boxed::Box, error::Error, path::Path};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{self, Duration};
use util::{cleanup_directory, set_display, Apikey, Updated, Video};

mod config;
mod data;
mod util;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    set_display();
    let mut config = Config::new();
    let mut data = Data::new();
    let client = Client::new();

    // Load the configs
    println!("Loading configuration...");
    config.load().await?;
    println!("Loaded configuration: {:?}", config);
    println!("Loading data...");
    data.load().await?;

    let mut mpv = start_mpv().await?;

    let _ = wait_for_api(&client, &config).await?;

    println!("API key is not set. Requesting a new API key...");
    config.key = Some(get_new_key(&client, &mut config).await?.key);
    config.write().await?;

    // Get the videos if we've never updated
    if data.last_update.is_none() {
        let updated = sync(&client, &config).await?;
        update_videos(&client, &mut config, &mut data, updated).await?;
        println!("Data Updated: {:?}", updated);
    }

    let mut interval = time::interval(Duration::from_secs(20));
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;

    mpv.kill().await?;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let updated = sync(&client, &config).await?;
                match (updated, data.last_update, data.update_content) {
                    (Some(updated), Some(last_update), _) if updated > last_update => {
                        println!("Update Videos");
                        update_videos(&client, &mut config, &mut data, Some(updated)).await?;
                    }
                    (Some(updated), None, _) => {
                        println!("Updated: {:?}", updated);
                        println!("Data last updated: None");
                        update_videos(&client, &mut config, &mut data, Some(updated)).await?;
                    }
                    (_, _, Some(true)) => {
                        println!("Forced content update triggered.");
                        update_videos(&client, &mut config, &mut data, None).await?;
                    }
                    _ => {
                        println!("No updates available.");
                    }
                }
                
            

                // Restart mpv if it exits
                match mpv.try_wait() {
                    Ok(Some(_)) => {
                        mpv = start_mpv().await?;
                    },
                    Ok(None) => (),
                    Err(error) => eprintln!("Error waiting for mpv process: {error}"),
                }

                // Avoid restarting mpv too frequently
                time::sleep(Duration::from_secs(10)).await;
            }
            _ = terminate.recv() => {
                println!("Received SIGTERM, terminating...");
                mpv.kill().await?;
                break;
            }
            _ = interrupt.recv() => {
                println!("Received SIGINT, terminating...");
                mpv.kill().await?;
                break;
            }
            _ = hup.recv() => {
                println!("Received SIGHUP, reloading configuration...");
                config.load().await?;
                data.load().await?;
            }
        }
    }

    Ok(())
}

async fn wait_for_api(client: &Client, config: &Config) -> Result<bool, Box<dyn Error>> {
    let mut interval = time::interval(Duration::from_secs(1));
    loop {
        let res = client.get(format!("{}/health", config.url)).send().await;
        if let Ok(response) = res {
            match response.status() {
                StatusCode::OK => break,
                StatusCode::INTERNAL_SERVER_ERROR => {
                    println!("Server error. Retrying in 2 minutes...");
                    time::interval(Duration::from_secs(120)).tick().await;
                }
                _ => (),
            }
        }
        interval.tick().await;
    }
    Ok(true)
}

async fn start_mpv() -> Result<Child, Box<dyn Error>> {
    let image_display_duration = 10;
    let child = Command::new("mpv")
        .arg("--loop-playlist=inf")
        .arg("--volume=-1")
        .arg("--no-terminal")
        .arg("--fullscreen")
        .arg("--input-ipc-server=/tmp/mpvsocket")
        .arg(format!(
            "--image-display-duration={}",
            image_display_duration
        ))
        .arg(format!(
            "--playlist={}/.local/share/signage/playlist.txt",
            std::env::var("HOME")?
        ))
        .spawn()?;

    Ok(child)
}

async fn get_new_key(client: &Client, config: &mut Config) -> Result<Apikey, Box<dyn Error>> {
    println!("Loading configuration...");
    config.load().await?;
    println!("Requesting new key from: {}/get-new-key/{}", config.url, config.id);

    let res_text = client
        .get(format!("{}/get-new-key/{}", config.url, config.id))
        .basic_auth(&config.username, Some(&config.password))
        .send()
        .await?
        .text()
        .await?;

    println!("Raw API response: {}", res_text);

    let res: Apikey = serde_json::from_str(&res_text)?;
    println!("Received new API key: {}", res.key);

    config.key = Some(res.key.clone());
    config.write().await?;
    Ok(res)
}


async fn sync(client: &Client, config: &Config) -> Result<Option<DateTime<Utc>>, Box<dyn Error>> {
    let res: Updated = client
        .get(format!("{}/sync/{}", config.url, config.id))
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .send()
        .await?
        .json()
        .await?;
    println!("Last updated: {:?}", res);
    Ok(res.updated)
}

async fn receive_videos(
    client: &Client,
    config: &mut Config,
) -> Result<Vec<Video>, Box<dyn Error>> {
    let url = format!("{}/recieve-videos/{}", config.url, config.id);

    let new_key = get_new_key(client, config).await?;
    let auth_token = new_key.key;
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-cache")
        .header("Accept-Encoding", "gzip, deflate, br")
        .header("Connection", "keep-alive")
        .header("APIKEY", auth_token)
        .send()
        .await?;

    let status = response.status();
    let text = response.text().await?;

    if status.is_success() {
        let res: Vec<Video> = serde_json::from_str(&text)?;
        Ok(res)
    } else {
        Err(format!("Failed to receive videos: {}", text).into())
    }
}

async fn update_videos(
    client: &Client,
    config: &mut Config,
    data: &mut Data,
    updated: Option<DateTime<Utc>>,
) -> Result<(), Box<dyn Error>> {
    data.videos = receive_videos(client, config).await?;
    data.last_update = updated;
    data.update_content= Some(false);
    data.write().await?;
    let home = std::env::var("HOME")?;

    if Path::new(&format!("{home}/.local/share/signage/playlist.txt")).try_exists()? {
        tokio::fs::remove_file(format!("{home}/.local/share/signage/playlist.txt")).await?;
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{home}/.local/share/signage/playlist.txt"))
        .await?;

    for video in data.videos.clone() {
        if !video.in_whitelist() {
            continue;
        }
        let file_path = video.download(client).await?;
        file.write_all(format!("{}\n", file_path).as_bytes())
            .await?;
    }
    cleanup_directory(&format!("{}/.local/share/signage", home)).await?;
    Ok(())
}
