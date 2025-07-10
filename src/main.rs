use chrono::{DateTime, Utc};
use config::Config;
use data::Data;
use reqwest::{Client, StatusCode};
use std::{boxed::Box, error::Error, path::Path};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{self, Duration};
use util::{cleanup_directory, set_display, Apikey, Updated, Video, ClientTimelineScheduleResponse};
use uuid::Uuid;

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
    if data.update_content.unwrap_or(false) {
        let updated = sync(&client, &config).await?;
        update_videos(&client, &mut config, &mut data, updated).await?;
        println!("Data Updated: {:?}", updated);
    }
    

    // Initialize with default polling interval
    let mut poll_interval = Duration::from_secs(60);
    let mut interval = time::interval(poll_interval);
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;

    mpv.kill().await?;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                println!("\n=== Checking for updates ===");
                let mut content_updated = false;
                
                // Try new schedule-aware system first
                match check_timeline_schedule(&client, &config).await {
                    Ok(schedule_response) => {
                        println!("✅ Using timeline schedule system");
                        
                        // Process the schedule response
                        content_updated = process_schedule_response(
                            &client, 
                            &mut config, 
                            &mut data, 
                            schedule_response.clone()
                        ).await?;
                        
                        // Calculate optimal polling interval based on schedule timing
                        let new_interval = calculate_poll_interval(&schedule_response);
                        if new_interval != poll_interval {
                            poll_interval = new_interval;
                            interval = time::interval(poll_interval);
                            println!("📊 Updated polling interval to {:?}", poll_interval);
                        }
                    }
                    Err(err) => {
                        println!("⚠️ Schedule check failed: {}, falling back to legacy sync", err);
                        
                        // Fall back to legacy sync system
                        let updated = sync(&client, &config).await?;
                        match (updated, data.last_update, data.update_content) {
                            (Some(updated), Some(last_update), _) if updated > last_update => {
                                println!("🔄 Legacy update detected");
                                update_videos(&client, &mut config, &mut data, Some(updated)).await?;
                                content_updated = true;
                            }
                            (Some(updated), None, _) => {
                                println!("🔄 Legacy initial update");
                                update_videos(&client, &mut config, &mut data, Some(updated)).await?;
                                content_updated = true;
                            }
                            _ => {
                                println!("📋 No legacy updates available");
                            }
                        }
                        
                        // Check legacy update_content flag
                        if data.update_content.unwrap_or(false) {
                            let updated = sync(&client, &config).await?;
                            update_videos(&client, &mut config, &mut data, updated).await?;
                            content_updated = true;
                            println!("🔄 Legacy content flag update");
                        }
                        
                        // Use default polling for legacy fallback
                        if poll_interval != Duration::from_secs(20) {
                            poll_interval = Duration::from_secs(20);
                            interval = time::interval(poll_interval);
                            println!("📊 Using legacy polling interval: 20s");
                        }
                    }
                }
                
                if content_updated {
                    println!("✅ Content updated successfully");
                    
                    // Force MPV restart to pick up new playlist immediately
                    println!("🔄 Restarting MPV to load new playlist...");
                    mpv.kill().await?;
                    mpv = start_mpv().await?;
                    println!("🎬 MPV restarted with new playlist");
                } else {
                    println!("📋 No content changes needed");
                }

                // Restart mpv if it exits
                match mpv.try_wait() {
                    Ok(Some(_)) => {
                        mpv = start_mpv().await?;
                        println!("🎬 Restarted mpv process");
                    },
                    Ok(None) => (),
                    Err(error) => eprintln!("❌ Error waiting for mpv process: {error}"),
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
        println!("Raw response: {}", text);
        let res: Vec<Video> = serde_json::from_str(&text)?;
        Ok(res)
    } else {
        Err(format!("Failed to receive videos: {}", text).into())
    }
}

// Removed: receive_videos_for_playlist - using legacy endpoint instead

async fn update_videos(
    client: &Client,
    config: &mut Config,
    data: &mut Data,
    updated: Option<DateTime<Utc>>,
) -> Result<(), Box<dyn Error>> {
    data.videos = receive_videos(client, config).await?;
    data.videos.sort_by_key(|v| v.asset_order);

    println!("{:#?}",  data.videos);
    let message = "==========================================================  Reduced";
    println!("{}", message);
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

// Removed: update_videos_for_playlist - using legacy endpoint instead

// ============================================================================
// NEW TIMELINE SCHEDULE FUNCTIONS
// ============================================================================

/// Check timeline schedule and update flags from the server
async fn check_timeline_schedule(
    client: &Client,
    config: &Config,
) -> Result<ClientTimelineScheduleResponse, Box<dyn Error>> {
    let url = format!("{}/client-timeline-schedule/{}", config.url, config.id);
    
    let response = client
        .get(&url)
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .send()
        .await?;

    if response.status().is_success() {
        let schedule_response: ClientTimelineScheduleResponse = response.json().await?;
        println!("Timeline Schedule Response: {:?}", schedule_response);
        Ok(schedule_response)
    } else {
        Err(format!("Failed to get timeline schedule: {}", response.status()).into())
    }
}

/// Determine if playlist needs updating based on schedule response
fn playlist_changed(
    current_playlist: Option<Uuid>, 
    schedule_response: &ClientTimelineScheduleResponse
) -> (bool, Option<Uuid>) {
    // Convert string UUIDs from API to Uuid for comparison
    let active_playlist_uuid = schedule_response.active_playlist_id
        .as_ref()
        .and_then(|s| s.parse::<Uuid>().ok());
    
    let changed = current_playlist != active_playlist_uuid;
    (changed, active_playlist_uuid)
}

/// Calculate optimal polling interval based on schedule timing
fn calculate_poll_interval(schedule_response: &ClientTimelineScheduleResponse) -> Duration {
    let now = Utc::now();
    
    // Check if current schedule is ending soon
    if let Some(schedule_ends) = schedule_response.schedule_ends_at {
        let seconds_until_end = (schedule_ends - now).num_seconds();
        if seconds_until_end > 0 && seconds_until_end < 300 { // Less than 5 minutes
            println!("Schedule ending in {} seconds, polling frequently", seconds_until_end);
            return Duration::from_secs(10); // Poll every 10 seconds
        }
    }
    
    // Check if next schedule is starting soon
    if let Some(next_starts) = schedule_response.next_schedule_starts_at {
        let seconds_until_start = (next_starts - now).num_seconds();
        if seconds_until_start > 0 && seconds_until_start < 300 { // Less than 5 minutes
            println!("Next schedule starting in {} seconds, polling frequently", seconds_until_start);
            return Duration::from_secs(10); // Poll every 10 seconds
        }
    }
    
    // Check if any update flags are set
    if schedule_response.update_flags.playlist_update_needed ||
       schedule_response.update_flags.schedule_update_needed ||
       schedule_response.update_flags.content_update_needed {
        println!("Update flags detected, polling more frequently");
        return Duration::from_secs(5); // Poll every 5 seconds when updates pending
    }
    
    // Check if we have active or upcoming schedules (poll more frequently during active periods)
    if schedule_response.schedule_ends_at.is_some() || schedule_response.next_schedule_starts_at.is_some() {
        return Duration::from_secs(30); // Poll every 30 seconds when schedules are active
    }
    
    // Default polling interval - no schedules active
    Duration::from_secs(60) // Poll every minute
}

/// Process schedule response and update data if needed
async fn process_schedule_response(
    client: &Client,
    config: &mut Config,
    data: &mut Data,
    schedule_response: ClientTimelineScheduleResponse,
) -> Result<bool, Box<dyn Error>> {
    let (playlist_changed, new_playlist) = playlist_changed(data.current_playlist, &schedule_response);
    let mut content_updated = false;
    
    // Update data with schedule information
    data.active_schedule_ends = schedule_response.schedule_ends_at;
    data.next_schedule_starts = schedule_response.next_schedule_starts_at;
    data.next_playlist_id = schedule_response.next_playlist_id
        .as_ref()
        .and_then(|s| s.parse::<Uuid>().ok());
    data.fallback_playlist = schedule_response.fallback_playlist_id
        .as_ref()
        .and_then(|s| s.parse::<Uuid>().ok());
    data.last_schedule_check = Some(Utc::now());
    
    // Check if we need to update content
    let needs_content_update = playlist_changed || 
                              schedule_response.update_flags.playlist_update_needed ||
                              schedule_response.update_flags.content_update_needed;
    
    if needs_content_update {
        println!("Content update needed - playlist changed: {}, flags: {:?}", 
                playlist_changed, schedule_response.update_flags);
        
        // Update current playlist
        data.current_playlist = new_playlist;
        
        // Always use the legacy endpoint which works for both scheduled and direct playlists
        let updated = sync(client, config).await?;
        update_videos(client, config, data, updated).await?;
        
        // Acknowledge the updates
        acknowledge_updates(client, config, &schedule_response.update_flags).await?;
        
        content_updated = true;
        println!("Content successfully updated");
    } else {
        println!("No content update needed - playlist: {:?}, flags: {:?}", 
                data.current_playlist, schedule_response.update_flags);
    }
    
    // Always save the updated schedule information
    data.write().await?;
    
    Ok(content_updated)
}

/// Acknowledge processed updates to the server
async fn acknowledge_updates(
    client: &Client,
    config: &Config,
    update_flags: &util::ClientUpdateFlagsResponse,
) -> Result<(), Box<dyn Error>> {
    if !update_flags.playlist_update_needed && 
       !update_flags.schedule_update_needed && 
       !update_flags.content_update_needed {
        return Ok(()); // Nothing to acknowledge
    }
    
    let url = format!("{}/client-acknowledge-updates/{}", config.url, config.id);
    
    let payload = serde_json::json!({
        "playlist_updated": update_flags.playlist_update_needed,
        "schedule_updated": update_flags.schedule_update_needed,
        "content_updated": update_flags.content_update_needed
    });
    
    let response = client
        .post(&url)
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .json(&payload)
        .send()
        .await?;
    
    if response.status().is_success() {
        println!("Successfully acknowledged updates");
    } else {
        println!("Failed to acknowledge updates: {}", response.status());
    }
    
    Ok(())
}
