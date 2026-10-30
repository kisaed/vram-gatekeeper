//! Extracts facts from llama-server's log output (spec §9.1 step 8, §13.2).
//!
//! Responsibility: recognise the "offloaded X/Y layers to GPU" line so the
//! overflow rule can tell when llama.cpp's auto-fit moved layers to the CPU.
//! Not responsible for: acting on the result (see `gate::controller`).
//!
//! The exact wording is confirmed against the bundled engine in the Phase 0
//! spike; the parser accepts every spelling seen so far and simply returns
//! `None` for anything else. A parse miss disables the layer check rather
//! than guessing (spec §27, first risk). Driver-spill detection still works.
//!
//! Implements FR-VRAM-4.

/// Parses a GPU layer placement from one log line.
///
/// Recognised shapes, from different llama.cpp versions:
/// - `load_tensors: offloaded 33/33 layers to GPU`
/// - `llm_load_tensors: offloaded 20/33 layers to GPU`
///
/// Returns `(offloaded, total)`.
pub fn parse_layer_placement(line: &str) -> Option<(u32, u32)> {
    let idx = line.find("offloaded ")?;
    let rest = &line[idx + "offloaded ".len()..];
    let (fraction, tail) = rest.split_once(' ')?;
    if !tail.trim_start().starts_with("layers") {
        return None;
    }
    let (off, total) = fraction.split_once('/')?;
    Some((off.parse().ok()?, total.parse().ok()?))
}

/// The speculative type named by the engine's `adding speculative
/// implementation '<type>'` line (spec §10.6), e.g. `draft-simple`.
///
/// Printed once for each type that started, as in
/// `common_speculative_init: adding speculative implementation 'ngram-mod'`.
pub fn parse_speculative_implementation(line: &str) -> Option<String> {
    const MARKER: &str = "adding speculative implementation '";
    let rest = &line[line.find(MARKER)? + MARKER.len()..];
    let name = &rest[..rest.find('\'')?];
    (!name.is_empty()).then(|| name.to_owned())
}

/// True if a log line says the listening socket could not be bound, which
/// happens when another process grabs the port between our `free_port` probe
/// and the child's own bind (spec §21).
pub fn is_bind_failure(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("couldn't bind")
        || l.contains("failed to bind")
        || l.contains("address already in use")
        || l.contains("only one usage of each socket address")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_failures_are_recognised_in_real_and_fake_wording() {
        assert!(is_bind_failure("error: couldn't bind HTTP server socket, hostname: 127.0.0.1, port: 8080"));
        assert!(is_bind_failure(
            "bind: Only one usage of each socket address (protocol/network address/port) is normally permitted."
        ));
        assert!(!is_bind_failure("load_tensors: offloaded 25/25 layers to GPU"));
    }

    #[test]
    fn parses_the_speculative_implementation_line() {
        assert_eq!(
            parse_speculative_implementation(
                "common_speculative_init: adding speculative implementation 'draft-simple'"
            ),
            Some("draft-simple".to_owned())
        );
        assert_eq!(
            parse_speculative_implementation(
                "[12:00:01] spec          x: adding speculative implementation 'ngram-mod'\r"
            ),
            Some("ngram-mod".to_owned())
        );
        assert_eq!(parse_speculative_implementation("adding speculative implementation ''"), None);
        assert_eq!(parse_speculative_implementation("adding speculative implementation 'unterminated"), None);
        assert_eq!(parse_speculative_implementation("load_tensors: offloaded 25/25 layers to GPU"), None);
    }

    #[test]
    fn parses_current_and_legacy_spellings() {
        assert_eq!(parse_layer_placement("load_tensors: offloaded 33/33 layers to GPU"), Some((33, 33)));
        assert_eq!(parse_layer_placement("llm_load_tensors: offloaded 20/33 layers to GPU"), Some((20, 33)));
        assert_eq!(parse_layer_placement("[12:00:01] load_tensors: offloaded 0/33 layers to GPU"), Some((0, 33)));
    }

    #[test]
    fn ignores_unrelated_lines() {
        assert_eq!(parse_layer_placement("llama_model_loader: loaded meta data"), None);
        assert_eq!(parse_layer_placement("offloaded some stuff"), None);
        assert_eq!(parse_layer_placement("offloaded x/y layers"), None);
        assert_eq!(parse_layer_placement(""), None);
    }
}
