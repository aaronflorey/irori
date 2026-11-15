//! The logo of the emulator each ROM game plays with, for the covers.

use std::collections::HashMap;
use std::sync::Arc;

use irori_core::GameId;
use irori_engines::{EngineRef, play};

use crate::Backend;

/// Logo addresses by game, for the games whose emulator has one.
#[derive(Debug, Default, PartialEq)]
pub struct EngineIcons(HashMap<GameId, Arc<str>>);

impl EngineIcons {
    pub fn get(&self, id: &GameId) -> Option<&Arc<str>> {
        self.0.get(id)
    }
}

impl Backend {
    /// The emulator logo of every ROM game in the library: the game's own
    /// choice, else its system's default, as a launch would pick. Reads the
    /// disk to see what is installed, once per system.
    pub async fn engine_icons(&self) -> EngineIcons {
        let (library, settings) = (self.library(), self.settings.clone());
        self.spawn_blocking(move || {
            let app = settings.app();
            let mut defaults: HashMap<&str, Option<EngineRef>> = HashMap::new();
            let icons = library
                .all_games()
                .filter(|g| g.runner().plays_roms())
                .filter_map(|game| {
                    let system = game.system.as_deref()?;
                    let id = game.id();
                    let own = settings.game(&id).engine;
                    let choice = if own.is_empty() {
                        defaults
                            .entry(system)
                            .or_insert_with(|| play::choice_for(&app, system, ""))
                            .clone()
                    } else {
                        EngineRef::parse(&own)
                    }?;
                    let icon = irori_engines::engine(&choice.engine)?
                        .icon
                        .as_deref()?
                        .into();
                    Some((id, icon))
                })
                .collect();
            EngineIcons(icons)
        })
        .await
        .unwrap_or_default()
    }
}
