//! Pure logic behind the tray icon: what to say and which menu items are
//! enabled (spec §17.2).
//!
//! Responsibility: derive the tooltip and menu availability from a status
//! snapshot. No Tauri types, so it is unit tested. The icon colour is the
//! snapshot's own `gate` indicator, which the window shows too.
//! Not responsible for: drawing the icon or building the menu.

use gatekeeper_core::gate::controller::StatusSnapshot;
use gatekeeper_core::gate::model_state::ModelStateKind;

/// One-line tooltip, for example `Gate closed: eldenring.exe · 0 models loaded`.
pub fn tooltip(s: &StatusSnapshot) -> String {
    let loaded = s.models.iter().filter(|m| m.state == ModelStateKind::Ready).count();
    let models = format!("{loaded} model{} loaded", if loaded == 1 { "" } else { "s" });
    let head = if s.paused {
        "Paused".to_owned()
    } else if s.gate_closed() {
        format!("Gate closed: {}", s.game.as_deref().unwrap_or("a game"))
    } else if s.session_active {
        format!("Allowed anyway: {}", s.game.as_deref().unwrap_or("a game"))
    } else {
        "Gate open".to_owned()
    };
    format!("VRAM Gatekeeper - {head} \u{b7} {models}")
}

/// Which optional menu items are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuAvailability {
    /// "Allow anyway" is shown only while a game gate is active.
    pub allow_anyway: bool,
    /// "Release held models" only while something is held.
    pub release_held: bool,
    /// True when the next click on the pause item should resume.
    pub resume: bool,
    /// True if there is anything to unload.
    pub unload_all: bool,
}

/// Computes menu availability.
pub fn menu_availability(s: &StatusSnapshot) -> MenuAvailability {
    MenuAvailability {
        allow_anyway: s.gate_closed(),
        release_held: s.models.iter().any(|m| m.state == ModelStateKind::Held || m.manual_hold),
        resume: s.paused,
        unload_all: s.models.iter().any(|m| m.state.has_process()),
    }
}

/// Total in-flight requests, used by the quit confirmation.
pub fn inflight_total(s: &StatusSnapshot) -> usize {
    s.models.iter().map(|m| m.inflight).sum()
}

#[cfg(test)]
mod tests {
    use gatekeeper_core::gate::controller::ModelStatus;
    use gatekeeper_core::gate::model_state::GateIndicator;

    use super::*;

    fn model(id: &str, state: ModelStateKind) -> ModelStatus {
        ModelStatus {
            id: id.into(),
            display_name: String::new(),
            state,
            cause: None,
            blocked_reason: None,
            inflight: 0,
            queued: 0,
            footprint_mib: None,
            footprint_estimated: false,
            gpu_layers: None,
            overflow: None,
            needs_restart: false,
            manual_hold: false,
            pid: None,
            idle_seconds: 0,
            failure: None,
        }
    }

    fn status(models: Vec<ModelStatus>) -> StatusSnapshot {
        StatusSnapshot {
            gate: GateIndicator::Open,
            paused: false,
            game: None,
            game_source: None,
            session_active: false,
            override_active: false,
            cooling_down: false,
            models,
            vram: None,
        }
    }

    #[test]
    fn tooltip_matches_the_spec_example() {
        let mut s = status(vec![model("a", ModelStateKind::Unloaded)]);
        s.session_active = true;
        s.game = Some("eldenring.exe".into());
        assert_eq!(tooltip(&s), "VRAM Gatekeeper - Gate closed: eldenring.exe \u{b7} 0 models loaded");
        s.session_active = false;
        s.models = vec![model("a", ModelStateKind::Ready), model("b", ModelStateKind::Ready)];
        assert_eq!(tooltip(&s), "VRAM Gatekeeper - Gate open \u{b7} 2 models loaded");
    }

    #[test]
    fn menu_items_appear_only_when_relevant() {
        let mut s = status(vec![model("a", ModelStateKind::Unloaded)]);
        let m = menu_availability(&s);
        assert!(!m.allow_anyway && !m.release_held && !m.resume && !m.unload_all);
        s.session_active = true;
        s.models[0].state = ModelStateKind::Held;
        s.paused = true;
        let m = menu_availability(&s);
        assert!(m.allow_anyway && m.release_held && m.resume);
    }

    #[test]
    fn inflight_requests_are_summed_for_the_quit_prompt() {
        let mut a = model("a", ModelStateKind::Ready);
        a.inflight = 2;
        let mut b = model("b", ModelStateKind::Ready);
        b.inflight = 1;
        assert_eq!(inflight_total(&status(vec![a, b])), 3);
    }
}
