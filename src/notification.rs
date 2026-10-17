use std::{process::Command, time::Duration};

#[cfg(any(target_os = "linux", test))]
use std::borrow::Cow;

pub(crate) struct Notifier;

impl Notifier {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn send(&mut self, title: &str, body: &str) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            const DISPLAY_NOTIFICATION: &str = "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run";

            let mut command = Command::new("osascript");
            command.args(["-e", DISPLAY_NOTIFICATION, title, body]);
            crate::command::output(&mut command, Duration::from_secs(3)).map(|_| ())
        }

        #[cfg(target_os = "linux")]
        {
            let title = escape_markup(title);
            let body = escape_markup(body);
            let mut command = Command::new("notify-send");
            command.args(["--", title.as_ref(), body.as_ref()]);
            crate::command::output(&mut command, Duration::from_secs(3)).map(|_| ())
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err("native notifications are unsupported on this platform".to_owned())
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn escape_markup(value: &str) -> Cow<'_, str> {
    if !value.contains(['&', '<', '>', '\'', '"']) {
        return Cow::Borrowed(value);
    }

    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\'' => escaped.push_str("&apos;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(character),
        }
    }
    Cow::Owned(escaped)
}

#[cfg(test)]
mod tests {
    use super::escape_markup;

    #[test]
    fn markup_escape_preserves_literal_notification_text() {
        assert_eq!(
            escape_markup("<agent & 'quoted' \"path\">").as_ref(),
            "&lt;agent &amp; &apos;quoted&apos; &quot;path&quot;&gt;"
        );
    }
}
