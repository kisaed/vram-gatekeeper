//! The global gate: why models are currently blocked (spec §7.1).
//!
//! Responsibility: combine game signals from several detectors into one
//! "game session" with a cooldown, and track pause and override.
//! Not responsible for: unloading models or timers (see `controller`, which
//! owns the clock and acts on the effects returned here).
//!
//! Implements FR-GAME-1, FR-GAME-5, FR-GAME-6, FR-APP-3.

use std::collections::BTreeMap;

use crate::detect::GameSignal;

/// What a game signal changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalEffect {
    /// Nothing the controller must act on.
    None,
    /// A new game session began: unload affected models.
    SessionStarted,
    /// Every source went quiet: start the cooldown timer with this id.
    StartCooldown(u64),
}

/// Current gate reasons.
#[derive(Debug, Default, Clone)]
pub struct GateState {
    /// The user paused the gatekeeper (blocks every model).
    pub paused: bool,
    /// Sources currently reporting a game: source name -> game name.
    sources: BTreeMap<String, String>,
    /// A game session is open (from the first positive signal until the
    /// cooldown completes).
    session_active: bool,
    /// "Allow anyway": the game does not block until the session ends.
    override_active: bool,
    /// Id of the live cooldown timer; older timers are ignored.
    cooldown_id: u64,
    /// A cooldown timer is running.
    cooldown_pending: bool,
    /// Most recent game (name, source), kept through the cooldown so error
    /// messages can still name it.
    last_game: Option<(String, String)>,
}

impl GateState {
    /// Applies one detector report.
    pub fn signal(&mut self, sig: &GameSignal) -> SignalEffect {
        if sig.active {
            let name = sig.name.clone().unwrap_or_else(|| "a game".to_owned());
            self.last_game = Some((name.clone(), sig.source.clone()));
            self.sources.insert(sig.source.clone(), name);
            // Any positive signal cancels a running cooldown: the game is
            // back (a launcher restarting its exe, for example).
            self.cooldown_pending = false;
            if !self.session_active {
                self.session_active = true;
                self.override_active = false;
                return SignalEffect::SessionStarted;
            }
            return SignalEffect::None;
        }

        self.sources.remove(&sig.source);
        if self.sources.is_empty() && self.session_active && !self.cooldown_pending {
            self.cooldown_pending = true;
            self.cooldown_id += 1;
            return SignalEffect::StartCooldown(self.cooldown_id);
        }
        SignalEffect::None
    }

    /// Ends the session if `id` is the live cooldown and no source came back.
    /// Returns true when the session ended.
    pub fn cooldown_elapsed(&mut self, id: u64) -> bool {
        if self.cooldown_pending && id == self.cooldown_id && self.sources.is_empty() {
            self.cooldown_pending = false;
            self.session_active = false;
            self.override_active = false;
            return true;
        }
        false
    }

    /// True while a game blocks models that unload during games.
    pub fn game_blocks(&self) -> bool {
        self.session_active && !self.override_active
    }

    /// True while a game session is open, even if overridden.
    pub fn session_active(&self) -> bool {
        self.session_active
    }

    /// True if "allow anyway" is in effect.
    pub fn override_active(&self) -> bool {
        self.override_active
    }

    /// True while the cooldown timer runs.
    pub fn cooling_down(&self) -> bool {
        self.cooldown_pending
    }

    /// Chooses "allow anyway" for the rest of this session.
    pub fn allow_anyway(&mut self) {
        if self.session_active {
            self.override_active = true;
        }
    }

    /// Name and source of the most recent game, for messages. Kept through
    /// the cooldown so errors can still name the game.
    pub fn current_game(&self) -> Option<(&str, &str)> {
        self.last_game.as_ref().map(|(name, src)| (name.as_str(), src.as_str()))
    }

    /// The current game, only while a session is open (not during the cooldown tail).
    pub fn active_game(&self) -> Option<(&str, &str)> {
        self.current_game().filter(|_| self.session_active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_positive_starts_session_second_does_not() {
        let mut g = GateState::default();
        assert_eq!(g.signal(&GameSignal::active("process_list", "a.exe")), SignalEffect::SessionStarted);
        assert_eq!(g.signal(&GameSignal::active("steam", "A")), SignalEffect::None);
        assert!(g.game_blocks());
    }

    #[test]
    fn stays_closed_until_all_sources_negative_then_cooldown() {
        let mut g = GateState::default();
        g.signal(&GameSignal::active("process_list", "a.exe"));
        g.signal(&GameSignal::active("steam", "A"));
        // One source exits: gate stays closed, no cooldown (spec §21).
        assert_eq!(g.signal(&GameSignal::inactive("process_list")), SignalEffect::None);
        assert!(g.game_blocks());
        let SignalEffect::StartCooldown(id) = g.signal(&GameSignal::inactive("steam")) else {
            panic!("expected cooldown");
        };
        assert!(g.game_blocks(), "still closed during cooldown");
        assert!(g.cooldown_elapsed(id));
        assert!(!g.game_blocks());
    }

    #[test]
    fn game_returning_cancels_cooldown() {
        let mut g = GateState::default();
        g.signal(&GameSignal::active("process_list", "a.exe"));
        let SignalEffect::StartCooldown(id) = g.signal(&GameSignal::inactive("process_list")) else {
            panic!("expected cooldown");
        };
        // Launcher restarts the exe inside the cooldown: same session.
        assert_eq!(g.signal(&GameSignal::active("process_list", "a.exe")), SignalEffect::None);
        assert!(!g.cooldown_elapsed(id), "stale cooldown must not reopen the gate");
        assert!(g.game_blocks());
    }

    #[test]
    fn override_lasts_only_for_the_session() {
        let mut g = GateState::default();
        g.allow_anyway(); // no session: ignored
        assert!(!g.override_active());
        g.signal(&GameSignal::active("p", "a.exe"));
        g.allow_anyway();
        assert!(!g.game_blocks());
        let SignalEffect::StartCooldown(id) = g.signal(&GameSignal::inactive("p")) else {
            panic!("expected cooldown");
        };
        assert!(g.cooldown_elapsed(id));
        assert_eq!(g.signal(&GameSignal::active("p", "a.exe")), SignalEffect::SessionStarted);
        assert!(g.game_blocks(), "a new session is blocked again");
    }
}
