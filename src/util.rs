use anyhow::Result;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{boxed::Box, error::Error, path::Path};
use tokio::process::Command;
use tokio::{
    fs::{self, File},
    io::{AsyncReadExt, AsyncWriteExt},
};

use std::env;

#[derive(Serialize, Deserialize, Clone)]
pub struct Apikey {
    pub key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Video {
    pub id: String,
    pub asset_url: String,
    #[serde(default)]
    pub asset_order: u8,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Updated {
    pub updated: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientTimelineScheduleResponse {
    pub active_playlist_id: Option<String>,
    pub fallback_playlist_id: Option<String>,
    pub schedule_ends_at: Option<DateTime<Utc>>,
    pub next_schedule_starts_at: Option<DateTime<Utc>>,
    pub next_playlist_id: Option<String>,
    pub update_flags: ClientUpdateFlagsResponse,
    pub layout: Option<String>,
    pub rotation: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientUpdateFlagsResponse {
    pub playlist_update_needed: bool,
    pub schedule_update_needed: bool,
    pub content_update_needed: bool,
    pub layout_change: bool,
    pub current_layout: Option<String>,
    pub current_rotation: Option<i32>,
}

impl Video {
    /// Downloads videos or images to `$HOME/.local/share/signage`
    pub async fn download(&self, client: &Client) -> Result<String, Box<dyn std::error::Error>> {
        // Extract the file extension from the URL
        let path = Path::new(&self.asset_url);
        let extension = path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("bin");
        // Clean up the directory after a successful download

        let file_path = format!(
            "{}/.local/share/signage/{}.{}",
            std::env::var("HOME")?,
            self.id,
            extension
        );

        // Check if the file already exists
        if Path::new(&file_path).exists() {
            println!("File already exists: {}", file_path);
            return Ok(file_path);
        }

        // Proceed with downloading the file
        let mut stream = client.get(&self.asset_url).send().await?.bytes_stream();
        let mut file = File::create(&file_path).await?;

        while let Some(content) = stream.next().await {
            tokio::io::copy(&mut content?.as_ref(), &mut file).await?;
        }

        println!("Downloaded to: {}", file_path);

        Ok(file_path)
    }

    pub fn in_whitelist(&self) -> bool {
        let whitelist = ["s3.amazonaws.com"];

        for url in whitelist {
            if self.asset_url.contains(url) {
                return true;
            } else {
                println!("URL not in whitelist: {}", self.asset_url);
            }
        }

        false
    }
}

/// Loads json from `dir/filename` into `T`
pub async fn load_json<T: Serialize + DeserializeOwned>(
    json: &mut T,
    dir: &str,
    filename: &str,
) -> Result<(), Box<dyn Error>> {
    if Path::new(&format!("{dir}/{filename}")).try_exists()? {
        let mut file = File::open(format!("{dir}/{filename}")).await?;
        let mut contents = vec![];
        file.read_to_end(&mut contents).await?;
        *json = serde_json::from_slice(&contents)?;
    } else {
        fs::create_dir_all(dir).await?;
        write_json(json, &format!("{dir}/{filename}")).await?;
    }

    Ok(())
}

pub async fn run_command(
    command: &str,
    args: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new(command).args(args).output().await?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Writes json from `T` into `path`
pub async fn write_json<T: Serialize>(json: &T, path: &str) -> Result<(), Box<dyn Error>> {
    let mut file = File::create(path).await?;
    file.write_all(&serde_json::to_vec_pretty(&json)?).await?;

    Ok(())
}

/// Cleans up the signage directory by removing files not listed in playlist.txt
pub async fn cleanup_directory(dir: &str) -> Result<(), Box<dyn Error>> {
    // Read the playlist.txt file
    let playlist_path = format!("{}/playlist.txt", dir);
    let mut playlist_file = File::open(&playlist_path).await?;
    let mut playlist_contents = String::new();
    playlist_file.read_to_string(&mut playlist_contents).await?;

    // Collect all filenames listed in playlist.txt
    let playlist_files: Vec<String> = playlist_contents
        .lines()
        .map(|line| line.trim().to_string())
        .collect();

    // Read the directory contents
    let mut dir_entries = fs::read_dir(dir).await?;

    while let Some(entry) = dir_entries.next_entry().await? {
        let path = entry.path();
        if path.is_file() {
            let filename = path.file_name().unwrap().to_string_lossy().to_string();
            // Ignore playlist.txt and data.json
            println!("Getting Files: {:?}", filename);
            if filename != "playlist.txt" && filename != "data.json" {
                // Delete the file if it's not in playlist.txt
                if !playlist_files.iter().any(|f| f.contains(&filename)) {
                    println!("Deleting file: {}", filename);
                    fs::remove_file(path).await?;
                }
            }
        }
    }
    Ok(())
}

pub fn set_display() {
    // Set the DISPLAY environment variable for the current process
    env::set_var("DISPLAY", ":0");

    // Optionally, print the current environment variable to verify
    match env::var("DISPLAY") {
        Ok(val) => println!("DISPLAY is set to: {}", val),
        Err(e) => println!("Couldn't read DISPLAY: {}", e),
    }
}
