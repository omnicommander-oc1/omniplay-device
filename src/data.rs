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
    pub update_content: Option<bool>,
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
