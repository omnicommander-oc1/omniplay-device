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

/// Handle rotation changes by determining the correct rotation
fn get_rotation_for_device(rotation: i32) -> i32 {
    match rotation {
        0 => {
            println!("📺 Device rotation set to 0 degrees");
            0
        },
        90 => {
            println!("📱 Device rotation set to 90 degrees");
            90
        },
        180 => {
            println!("🔄 Device rotation set to 180 degrees");
            180
        },
        270 => {
            println!("🔄 Device rotation set to 270 degrees");
            270
        },
        _ => {
            println!("⚠️ Invalid rotation: {}, defaulting to 0 degrees", rotation);
            0 // Default to no rotation
        }
    }
}

/// Acknowledge layout update to the API
async fn acknowledge_layout_update(client: &Client, config: &Config) -> Result<(), Box<dyn Error>> {
    let url = format!("{}/client-acknowledge-updates/{}", config.url, config.id);
    
    let payload = serde_json::json!({
        "layout_updated": true
    });
    
    let response = client
        .post(&url)
        .header("APIKEY", config.key.clone().unwrap_or_default())
        .json(&payload)
        .send()
        .await?;

    if response.status().is_success() {
        println!("✅ Layout update acknowledged to API");
    } else {
        println!("⚠️ Failed to acknowledge layout update: {}", response.status());
    }
    
    Ok(())
}

/// Kill any existing MPV processes to prevent conflicts
async fn cleanup_mpv_processes() -> Result<(), Box<dyn Error>> {
    println!("🧹 Cleaning up any existing MPV processes...");
    
    // Kill all MPV processes
    let output = Command::new("pkill")
        .args(&["-f", "mpv"])
        .output()
        .await;
    
    match output {
        Ok(result) => {
            if result.status.success() {
                println!("✅ MPV processes cleaned up successfully");
            } else {
                println!("ℹ️ No existing MPV processes found to clean up");
            }
        }
        Err(e) => {
            println!("⚠️ Failed to clean up MPV processes: {}", e);
            // Don't fail the whole process, just log the warning
        }
    }
    
    // Give a small delay to ensure processes are fully terminated
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    Ok(())
}

/// Comprehensive cleanup at startup to ensure clean state
async fn startup_cleanup() -> Result<(), Box<dyn Error>> {
    println!("🧹 Performing startup cleanup...");
    
    // Kill all MPV processes
    cleanup_mpv_processes().await?;
    
    // Clean up any stale socket files
    let socket_path = "/tmp/mpvsocket";
    if Path::new(socket_path).exists() {
        if let Err(e) = tokio::fs::remove_file(socket_path).await {
            println!("⚠️ Failed to remove stale socket file: {}", e);
        } else {
            println!("✅ Removed stale socket file");
        }
    }
    
    println!("✅ Startup cleanup completed");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    set_display();
    
    // Perform comprehensive cleanup at startup
    startup_cleanup().await?;
    
    let mut config = Config::new();
    let mut data = Data::new();
    let client = Client::new();

    // Load the configs
    println!("Loading configuration...");
    config.load().await?;
    println!("Loaded configuration: {:?}", config);
    println!("Loading data...");
    data.load().await?;

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
    

    // Get initial rotation from API before starting MPV
    let initial_rotation = get_initial_rotation(&client, &config).await;
    
    // Store the initial rotation in data for future use
    data.current_rotation = Some(initial_rotation);
    data.rotation_applied = Some(initial_rotation);
    data.write().await?;
    
    // Initialize with default polling interval
    let mut poll_interval = Duration::from_secs(60);
    let mut interval = time::interval(poll_interval);
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;

    let mut mpv = start_mpv_with_rotation(initial_rotation).await?;

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
                        let (content_update_needed, mpv_restart_needed) = process_schedule_response(
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

                        // If MPV needs restart, restart it
                        if mpv_restart_needed {
                            println!("🔄 MPV needs restart due to changes. Restarting...");
                            mpv.kill().await?;
                            
                            // Determine the correct rotation based on current rotation setting
                            let rotation_degrees = if let Some(rotation) = data.current_rotation {
                                get_rotation_for_device(rotation)
                            } else {
                                0 // Default to no rotation
                            };
                            
                            mpv = start_mpv_with_rotation(rotation_degrees).await?;
                            println!("🎬 MPV restarted with rotation: {} degrees", rotation_degrees);
                        }
                        content_updated = content_update_needed;
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
                    
                    // Use current rotation when restarting MPV
                    let rotation_degrees = if let Some(rotation) = data.current_rotation {
                        get_rotation_for_device(rotation)
                    } else {
                        0 // Default to no rotation
                    };
                    
                    mpv = start_mpv_with_rotation(rotation_degrees).await?;
                    println!("🎬 MPV restarted with new playlist and rotation: {} degrees", rotation_degrees);
                } else {
                    println!("📋 No content changes needed");
                }

                // Restart mpv if it exits
                match mpv.try_wait() {
                    Ok(Some(_)) => {
                        // Use current rotation when restarting MPV
                        let rotation_degrees = if let Some(rotation) = data.current_rotation {
                            get_rotation_for_device(rotation)
                        } else {
                            0 // Default to no rotation
                        };
                        
                        mpv = start_mpv_with_rotation(rotation_degrees).await?;
                        println!("🎬 Restarted mpv process with rotation: {} degrees", rotation_degrees);
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

/// Fetch the initial rotation setting from the API
async fn get_initial_rotation(client: &Client, config: &Config) -> i32 {
    println!("🔍 Fetching initial rotation setting from API...");
    
    match check_timeline_schedule(client, config).await {
        Ok(schedule_response) => {
            if let Some(rotation) = schedule_response.rotation {
                let rotation_degrees = get_rotation_for_device(rotation);
                println!("✅ Initial rotation from API: {} degrees", rotation_degrees);
                return rotation_degrees;
            } else {
                println!("📺 No rotation setting found in API, using default (0 degrees)");
            }
        }
        Err(err) => {
            println!("⚠️ Failed to fetch initial rotation from API: {}, using default (0 degrees)", err);
        }
    }
    
    0 // Default to no rotation
}

async fn start_mpv() -> Result<Child, Box<dyn Error>> {
    start_mpv_with_rotation(0).await
}

async fn start_mpv_with_rotation(rotation_degrees: i32) -> Result<Child, Box<dyn Error>> {
    // Clean up any existing MPV processes first
    cleanup_mpv_processes().await?;
    
    let image_display_duration = 10;
    let mut cmd = Command::new("mpv");
    cmd.arg("--loop-playlist=inf")
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
        ));
    
    // Add rotation if specified
    if rotation_degrees != 0 {
        cmd.arg(format!("--video-rotate={}", rotation_degrees));
        println!("🔄 Starting MPV with rotation: {} degrees", rotation_degrees);
    } else {
        println!("📺 Starting MPV with no rotation (horizontal)");
    }
    
    let child = cmd.spawn()?;
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
    println!("🔍 Checking playlist changes:");
    println!("  Current playlist: {:?}", current_playlist);
    println!("  Active playlist ID: {:?}", schedule_response.active_playlist_id);
    println!("  Fallback playlist ID: {:?}", schedule_response.fallback_playlist_id);
    
    // Determine the effective playlist to use
    let effective_playlist = if let Some(active_id) = &schedule_response.active_playlist_id {
        // Use active playlist if available
        println!("  ✅ Using ACTIVE playlist: {}", active_id);
        active_id.parse::<Uuid>().ok()
    } else if let Some(fallback_id) = &schedule_response.fallback_playlist_id {
        // Use fallback playlist if no active playlist
        println!("  🔄 Using FALLBACK playlist: {}", fallback_id);
        fallback_id.parse::<Uuid>().ok()
    } else {
        // No playlist available
        println!("  ❌ No playlist available (neither active nor fallback)");
        None
    };
    
    let changed = current_playlist != effective_playlist;
    println!("  📊 Playlist changed: {} (from {:?} to {:?})", changed, current_playlist, effective_playlist);
    
    (changed, effective_playlist)
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
) -> Result<(bool, bool), Box<dyn Error>> {
    println!("📋 Processing schedule response...");
    let (playlist_changed, new_playlist) = playlist_changed(data.current_playlist, &schedule_response);
    let mut content_updated = false;
    let mut mpv_restart_needed = false;
    
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
    
    println!("📊 Schedule state:");
    println!("  Active ends at: {:?}", data.active_schedule_ends);
    println!("  Next starts at: {:?}", data.next_schedule_starts);
    println!("  Next playlist: {:?}", data.next_playlist_id);
    println!("  Fallback playlist: {:?}", data.fallback_playlist);
    
    // Check if we need to update content
    let needs_content_update = playlist_changed || 
                              schedule_response.update_flags.playlist_update_needed ||
                              schedule_response.update_flags.content_update_needed;
    
    if needs_content_update {
        println!("🔄 Content update needed - playlist changed: {}, flags: {:?}", 
                playlist_changed, schedule_response.update_flags);
        
        // Update current playlist
        data.current_playlist = new_playlist;
        println!("🎬 Setting current playlist to: {:?}", data.current_playlist);
        
        // Always use the legacy endpoint which works for both scheduled and direct playlists
        let updated = sync(client, config).await?;
        update_videos(client, config, data, updated).await?;
        
        // Acknowledge the updates
        acknowledge_updates(client, config, &schedule_response.update_flags).await?;
        
        content_updated = true;
        mpv_restart_needed = true; // Content changes require MPV restart
        println!("✅ Content successfully updated");
    } else {
        println!("📋 No content update needed - playlist: {:?}, flags: {:?}", 
                data.current_playlist, schedule_response.update_flags);
    }
    
    // Check for layout/rotation changes
    if schedule_response.update_flags.layout_change {
        println!("🔄 Layout/rotation change detected!");
        
        // Get the new layout and rotation from the response
        let new_layout = schedule_response.update_flags.current_layout
            .as_ref()
            .or(schedule_response.layout.as_ref());
        let new_rotation = schedule_response.rotation;
            
        // Handle layout changes
        if let Some(layout) = new_layout {
            println!("📱 Applying new layout: {}", layout);
            data.current_layout = Some(layout.clone());
            data.layout_applied = Some(layout.clone());
        }
        
        // Handle rotation changes
        if let Some(rotation) = new_rotation {
            println!("🔄 Applying new rotation: {} degrees", rotation);
            
            // Check if this is actually a different rotation than what we have applied
            let rotation_actually_changed = data.rotation_applied != Some(rotation);
            
            if rotation_actually_changed {
                let rotation_degrees = get_rotation_for_device(rotation);
                println!("🔄 Rotation requires MPV restart: {} degrees", rotation_degrees);
                
                // Update our tracking data
                data.current_rotation = Some(rotation);
                data.rotation_applied = Some(rotation);
                
                // Set flag to restart MPV with new rotation
                mpv_restart_needed = true;
                
                println!("✅ Rotation change will be applied on MPV restart");
            } else {
                println!("📋 Rotation is the same as currently applied ({}), no restart needed", rotation);
            }
        }
        
        // Acknowledge the layout/rotation update to the API
        if let Err(e) = acknowledge_layout_update(client, config).await {
            println!("⚠️ Failed to acknowledge layout/rotation update: {}", e);
        } else {
            println!("✅ Layout/rotation update acknowledged");
        }
    } else {
        // Update current layout/rotation tracking even if no change flag (for initialization)
        if let Some(layout) = schedule_response.layout.as_ref() {
            if data.current_layout.as_ref() != Some(layout) {
                println!("📱 Updating current layout tracking to: {}", layout);
                data.current_layout = Some(layout.clone());
                
                // If we've never applied a layout before, apply it now
                if data.layout_applied.is_none() {
                    println!("🔄 Initial layout setup: applying {}", layout);
                    data.layout_applied = Some(layout.clone());
                }
            }
        }
        
        if let Some(rotation) = schedule_response.rotation {
            if data.current_rotation != Some(rotation) {
                println!("🔄 Updating current rotation tracking to: {} degrees", rotation);
                data.current_rotation = Some(rotation);
                
                // If we've never applied a rotation before, apply it now
                if data.rotation_applied.is_none() {
                    println!("🔄 Initial rotation setup: applying {} degrees", rotation);
                    data.rotation_applied = Some(rotation);
                    mpv_restart_needed = true; // Need to restart with proper rotation
                    println!("✅ Initial rotation will be applied on MPV restart");
                }
            }
        }
    }
    
    // Always save the updated schedule information
    data.write().await?;
    
    Ok((content_updated, mpv_restart_needed))
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

