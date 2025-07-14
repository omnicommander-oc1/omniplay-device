use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{boxed::Box, env, error::Error};
use uuid::Uuid;
use crate::util::{load_json, write_json, Video};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Data {
    pub videos: Vec<Video>,
    pub last_update: Option<DateTime<Utc>>,
    pub current_playlist: Option<Uuid>,
    pub active_schedule_ends: Option<DateTime<Utc>>,
    pub next_schedule_starts: Option<DateTime<Utc>>,
    pub next_playlist_id: Option<Uuid>,
    pub fallback_playlist: Option<Uuid>,
    pub update_content: Option<bool>,
    pub last_schedule_check: Option<DateTime<Utc>>,
    pub current_layout: Option<String>,
    pub layout_applied: Option<String>, // Track the last layout that was successfully applied
    pub current_rotation: Option<i32>,
    pub rotation_applied: Option<i32>, // Track the last rotation that was successfully applied
}
impl Data {
    pub fn new() -> Self {
        Data::default()
    }

    /// Loads `Data` from $HOME/.local/share/signage/data.json
    pub async fn load(&mut self) -> Result<(), Box<dyn Error>> {
        println!("Reading data.json: ");
        load_json(
            self,
            &format!("{}/.local/share/signage", env::var("HOME")?),
            "data.json",
        )
        .await
    }
    /// Writes `Data` to $HOME/.local/share/signage/data.json
    pub async fn write(&self) -> Result<(), Box<dyn Error>> {
        println!("Writing to data.json:");
        write_json(
            self,
            &format!("{}/.local/share/signage/data.json", env::var("HOME")?),
        )
        .await
    }
}
