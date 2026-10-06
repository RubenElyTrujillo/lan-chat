// Shared clipboard bridge: pure bounds and gates for the clipboard feature.
// The Tauri command wiring (read/write commands, plugin init, global
// shortcut registration) lives in lib.rs; everything testable is here.

/// Max accepted clipboard text, counted in CHARS (not bytes): the bound
/// shared by `read_clipboard`, `write_clipboard` and the hub clipboard
/// payload.
pub const MAX_CLIPBOARD_TEXT: usize = 64_000;

/// Pure bound + trim semantics for clipboard text: trims surrounding
/// whitespace, rejects empty-after-trim and rejects anything over
/// `MAX_CLIPBOARD_TEXT` chars. Err("invalid-text") mirrors the hub send
/// gate so callers map one error shape.
pub fn bound_clipboard_text(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > MAX_CLIPBOARD_TEXT {
        return Err("invalid-text".to_string());
    }
    Ok(text.to_string())
}

/// Global-shortcut gate: only a Pressed transition triggers the frontend
/// request event. Released is the key-up bounce and must never emit.
#[cfg(desktop)]
pub fn clipboard_event_should_emit(state: tauri_plugin_global_shortcut::ShortcutState) -> bool {
    state == tauri_plugin_global_shortcut::ShortcutState::Pressed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_trims_whitespace_and_accepts_inner_content() {
        assert_eq!(bound_clipboard_text("  hola  "), Ok("hola".to_string()));
        assert_eq!(
            bound_clipboard_text("line1\n\nline2\t"),
            Ok("line1\n\nline2".to_string())
        );
    }

    #[test]
    fn bound_rejects_empty_and_whitespace_only() {
        assert_eq!(bound_clipboard_text(""), Err("invalid-text".to_string()));
        assert_eq!(
            bound_clipboard_text("   \n\t "),
            Err("invalid-text".to_string())
        );
    }

    #[test]
    fn bound_counts_chars_not_bytes_at_the_limit() {
        // 64_000 two-byte chars: 128_000 bytes but exactly at the bound.
        let multibyte = "é".repeat(MAX_CLIPBOARD_TEXT);
        assert_eq!(bound_clipboard_text(&multibyte), Ok(multibyte.clone()));
        // One more char, no matter the byte width, is over the bound.
        let over = "é".repeat(MAX_CLIPBOARD_TEXT + 1);
        assert_eq!(bound_clipboard_text(&over), Err("invalid-text".to_string()));
    }

    #[test]
    fn bound_accepts_exactly_ascii_max() {
        let exact = "x".repeat(MAX_CLIPBOARD_TEXT);
        assert_eq!(bound_clipboard_text(&exact), Ok(exact));
        let over = "x".repeat(MAX_CLIPBOARD_TEXT + 1);
        assert_eq!(bound_clipboard_text(&over), Err("invalid-text".to_string()));
    }

    #[cfg(desktop)]
    #[test]
    fn shortcut_gate_emits_only_on_pressed() {
        use tauri_plugin_global_shortcut::ShortcutState;
        assert!(clipboard_event_should_emit(ShortcutState::Pressed));
        assert!(!clipboard_event_should_emit(ShortcutState::Released));
    }
}
