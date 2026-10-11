use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::process::Command;

const INVENTORY_FORMAT: &str = "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{window_index}\t#{pane_index}\t#{pane_height}\t#{pane_width}\t#{pane_dead}\t#{pane_last_output_time}\t#{q:session_name}\t#{q:window_name}\t#{q:pane_current_path}";
const MAX_CAPTURE_LINES: usize = 200;
const MAX_CAPTURE_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Pane {
    pub(crate) session_id: String,
    pub(crate) window_id: String,
    pub(crate) pane_id: String,
    pub(crate) pane_pid: u32,
    pub(crate) window_index: u32,
    pub(crate) pane_index: u32,
    pub(crate) height: u32,
    pub(crate) width: u32,
    pub(crate) dead: bool,
    pub(crate) last_output: Option<u64>,
    pub(crate) session_name: String,
    pub(crate) window_name: String,
    pub(crate) path: PathBuf,
}

impl Pane {
    pub(crate) fn target(&self) -> String {
        format!("{}:{}.{}", self.session_id, self.window_id, self.pane_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Client {
    pub(crate) name: String,
    pub(crate) session_id: String,
    pub(crate) pane_id: String,
}

#[derive(Debug)]
pub(crate) enum TmuxError {
    Spawn(std::io::Error),
    Command(String),
    Protocol(String),
}

impl fmt::Display for TmuxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "could not run tmux: {error}"),
            Self::Command(message) => write!(formatter, "tmux: {message}"),
            Self::Protocol(message) => write!(formatter, "invalid tmux response: {message}"),
        }
    }
}

impl std::error::Error for TmuxError {}

#[derive(Debug, Default)]
pub(crate) struct Tmux;

impl Tmux {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn inventory(&self) -> Result<Vec<Pane>, TmuxError> {
        let output = self.run_control(["list-panes", "-a", "-F", INVENTORY_FORMAT])?;
        parse_inventory(&output)
    }

    pub(crate) fn capture(&self, pane: &Pane) -> Result<String, TmuxError> {
        let start = if pane.height >= MAX_CAPTURE_LINES as u32 {
            (pane.height - MAX_CAPTURE_LINES as u32).to_string()
        } else {
            format!("-{}", MAX_CAPTURE_LINES as u32 - pane.height)
        };
        let output = self.run_control([
            "capture-pane",
            "-p",
            "-C",
            "-T",
            "-S",
            &start,
            "-E",
            "-",
            "-t",
            &pane.pane_id,
        ])?;
        decode_capture(&output)
    }

    pub(crate) fn clients(&self) -> Result<Vec<Client>, TmuxError> {
        let output = self.run_control([
            "list-clients",
            "-F",
            "#{client_name}\t#{session_id}\t#{pane_id}",
        ])?;
        parse_clients(&output)
    }

    fn run_control<'a, const N: usize>(&self, command: [&'a str; N]) -> Result<Vec<u8>, TmuxError> {
        let output = Command::new("tmux")
            .arg("-N")
            .arg("-C")
            .args(command)
            .output()
            .map_err(TmuxError::Spawn)?;

        if !output.status.success() {
            let diagnostic = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(TmuxError::Command(if diagnostic.is_empty() {
                format!("command exited with {}", output.status)
            } else {
                diagnostic
            }));
        }

        parse_control(&output.stdout)
    }
}

/// Maps an agent's process chain to the nearest live tmux pane root.
pub(crate) fn map_agent(
    pids: &[u32],
    parents: &HashMap<u32, u32>,
    panes: &[Pane],
    origin_session: Option<&str>,
) -> Option<Pane> {
    let mut best_distance = None;
    let mut matching_pane_ids = HashSet::new();

    for &pid in pids {
        let mut distance = 0usize;
        let mut current = pid;
        let mut visited = HashSet::new();

        loop {
            if !visited.insert(current) {
                break;
            }

            for pane in panes
                .iter()
                .filter(|pane| !pane.dead && pane.pane_pid == current)
            {
                match best_distance {
                    None => {
                        best_distance = Some(distance);
                        matching_pane_ids.clear();
                        matching_pane_ids.insert(pane.pane_id.as_str());
                    }
                    Some(best) if distance < best => {
                        best_distance = Some(distance);
                        matching_pane_ids.clear();
                        matching_pane_ids.insert(pane.pane_id.as_str());
                    }
                    Some(best) if distance == best => {
                        matching_pane_ids.insert(pane.pane_id.as_str());
                    }
                    Some(_) => {}
                }
            }

            if best_distance == Some(distance) {
                break;
            }
            let Some(&parent) = parents.get(&current) else {
                break;
            };
            current = parent;
            distance += 1;
        }
    }

    let mut pane_ids: Vec<_> = matching_pane_ids.into_iter().collect();
    pane_ids.sort_by_key(|pane_id| stable_id_number(pane_id, '%').unwrap_or(u64::MAX));
    pane_ids.into_iter().find_map(|pane_id| {
        let linked: Vec<_> = panes
            .iter()
            .filter(|pane| !pane.dead && pane.pane_id == pane_id)
            .collect();
        choose_linked_pane(&linked, origin_session).cloned()
    })
}

/// Extracts the tracker-origin tmux session from `$TMUX`.
pub(crate) fn origin_session(value: &str) -> Option<String> {
    let (socket_and_server, session) = value.rsplit_once(',')?;
    let (socket, server) = socket_and_server.rsplit_once(',')?;
    if socket.is_empty()
        || !is_decimal(server)
        || server.parse::<u32>().is_err()
        || !is_decimal(session)
        || session.parse::<u64>().is_err()
    {
        return None;
    }
    Some(format!("${session}"))
}

/// Replaces terminal control characters while preserving line boundaries.
pub(crate) fn sanitize(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\n' => '\n',
            character if character.is_control() => '\u{fffd}',
            character => character,
        })
        .collect()
}

fn parse_control(stdout: &[u8]) -> Result<Vec<u8>, TmuxError> {
    if !stdout.ends_with(b"\n") {
        return Err(TmuxError::Protocol("truncated control stream".to_owned()));
    }
    let mut block = None::<ControlHeader>;
    let mut payload = Vec::new();
    let mut complete = false;

    let mut have_payload_line = false;
    for raw_line in stdout.split_inclusive(|byte| *byte == b'\n') {
        let line = if raw_line.last() == Some(&b'\n') {
            &raw_line[..raw_line.len() - 1]
        } else {
            raw_line
        };

        if let Some(header) = &block {
            if matches_guard(line, b"%end", header) {
                complete = true;
                block = None;
                continue;
            }
            if matches_guard(line, b"%error", header) {
                return Err(TmuxError::Command(control_error(&payload)));
            }
            if have_payload_line {
                payload.push(b'\n');
            }
            payload.extend_from_slice(line);
            have_payload_line = true;
        } else if let Some(header) = parse_header(line, b"%begin") {
            if complete {
                return Err(TmuxError::Protocol("multiple command blocks".to_owned()));
            }
            block = Some(header);
        }
    }

    if block.is_some() || !complete {
        return Err(TmuxError::Protocol(
            "incomplete control command block".to_owned(),
        ));
    }
    Ok(payload)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ControlHeader {
    time: Vec<u8>,
    number: Vec<u8>,
    flags: Vec<u8>,
}

fn parse_header(line: &[u8], marker: &[u8]) -> Option<ControlHeader> {
    let rest = line.strip_prefix(marker)?.strip_prefix(b" ")?;
    let mut parts = rest.split(|byte| *byte == b' ');
    let time = parts.next()?;
    let number = parts.next()?;
    let flags = parts.next()?;
    if time.is_empty()
        || number.is_empty()
        || flags.is_empty()
        || parts.next().is_some()
        || !time.iter().all(u8::is_ascii_digit)
        || !number.iter().all(u8::is_ascii_digit)
        || !flags.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    Some(ControlHeader {
        time: time.to_vec(),
        number: number.to_vec(),
        flags: flags.to_vec(),
    })
}

fn matches_guard(line: &[u8], marker: &[u8], header: &ControlHeader) -> bool {
    parse_header(line, marker).is_some_and(|candidate| candidate == *header)
}

fn control_error(payload: &[u8]) -> String {
    let diagnostic = String::from_utf8_lossy(payload).trim().to_owned();
    if diagnostic.is_empty() {
        "command failed".to_owned()
    } else {
        diagnostic
    }
}

fn parse_inventory(payload: &[u8]) -> Result<Vec<Pane>, TmuxError> {
    let records = split_tmux_records(payload)?;
    records.into_iter().map(parse_pane).collect()
}

fn split_tmux_records(payload: &[u8]) -> Result<Vec<Vec<Vec<u8>>>, TmuxError> {
    if payload.is_empty() {
        return Ok(Vec::new());
    }

    let mut records = Vec::new();
    let mut fields = Vec::new();
    let mut field = Vec::new();
    let mut escaped = false;

    for &byte in payload {
        if escaped {
            field.push(byte);
            escaped = false;
            continue;
        }
        if byte == b'\\' {
            field.push(byte);
            escaped = true;
        } else if byte == b'\t' {
            fields.push(std::mem::take(&mut field));
        } else if byte == b'\n' {
            fields.push(std::mem::take(&mut field));
            records.push(std::mem::take(&mut fields));
        } else {
            field.push(byte);
        }
    }

    if escaped {
        return Err(TmuxError::Protocol("dangling inventory escape".to_owned()));
    }
    if !field.is_empty() || !fields.is_empty() {
        fields.push(field);
        records.push(fields);
    }
    Ok(records)
}

fn parse_pane(fields: Vec<Vec<u8>>) -> Result<Pane, TmuxError> {
    if fields.len() != 13 {
        return Err(TmuxError::Protocol(format!(
            "inventory record has {} fields, expected 13",
            fields.len()
        )));
    }
    let session_id = stable_id(&fields[0], '$', "session ID")?;
    let window_id = stable_id(&fields[1], '@', "window ID")?;
    let pane_id = stable_id(&fields[2], '%', "pane ID")?;
    let pane_pid = decimal_u32(&fields[3], "pane PID")?;
    let window_index = decimal_u32(&fields[4], "window index")?;
    let pane_index = decimal_u32(&fields[5], "pane index")?;
    let height = decimal_u32(&fields[6], "pane height")?;
    let width = decimal_u32(&fields[7], "pane width")?;
    let dead = match fields[8].as_slice() {
        b"0" => false,
        b"1" => true,
        _ => return Err(TmuxError::Protocol("invalid pane dead flag".to_owned())),
    };
    let last_output = if fields[9].is_empty() {
        None
    } else {
        Some(decimal_u64(&fields[9], "pane last output time")?)
    };

    Ok(Pane {
        session_id,
        window_id,
        pane_id,
        pane_pid,
        window_index,
        pane_index,
        height,
        width,
        dead,
        last_output,
        session_name: decode_tmux_quoted(&fields[10], "session name")?,
        window_name: decode_tmux_quoted(&fields[11], "window name")?,
        path: PathBuf::from(decode_tmux_quoted(&fields[12], "pane path")?),
    })
}

fn parse_clients(payload: &[u8]) -> Result<Vec<Client>, TmuxError> {
    if payload.is_empty() {
        return Ok(Vec::new());
    }
    payload
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<_> = line.split(|byte| *byte == b'\t').collect();
            if fields.len() != 3 {
                return Err(TmuxError::Protocol("invalid client record".to_owned()));
            }
            let name = utf8(fields[0], "client name")?;
            if name.is_empty() {
                return Err(TmuxError::Protocol("invalid client name".to_owned()));
            }
            Ok(Client {
                name,
                session_id: stable_id(fields[1], '$', "client session ID")?,
                pane_id: stable_id(fields[2], '%', "client pane ID")?,
            })
        })
        .collect()
}

fn decode_capture(payload: &[u8]) -> Result<String, TmuxError> {
    let mut decoded = Vec::with_capacity(payload.len());
    let mut index = 0;
    while index < payload.len() {
        if payload[index] != b'\\' {
            decoded.push(payload[index]);
            index += 1;
            continue;
        }
        let Some(&next) = payload.get(index + 1) else {
            return Err(TmuxError::Protocol("dangling capture escape".to_owned()));
        };
        if let Some(octal) = payload
            .get(index + 1..index + 4)
            .filter(|digits| digits.iter().all(|digit| (b'0'..=b'7').contains(digit)))
        {
            let value = u16::from(octal[0] - b'0') * 64
                + u16::from(octal[1] - b'0') * 8
                + u16::from(octal[2] - b'0');
            decoded.push(
                u8::try_from(value)
                    .map_err(|_| TmuxError::Protocol("invalid capture octal byte".to_owned()))?,
            );
            index += 4;
        } else {
            decoded.push(next);
            index += 2;
        }
    }

    let mut start = 0;
    let mut separators = 0;
    for (offset, byte) in decoded.iter().enumerate().rev() {
        if *byte == b'\n' && offset + 1 != decoded.len() {
            separators += 1;
            if separators == MAX_CAPTURE_LINES {
                start = offset + 1;
                break;
            }
        }
    }
    let sanitized = sanitize(&String::from_utf8_lossy(&decoded[start..]));
    Ok(truncate_utf8(sanitized, MAX_CAPTURE_BYTES))
}

fn truncate_utf8(mut text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut boundary = limit;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    text
}

fn decode_tmux_quoted(field: &[u8], label: &str) -> Result<String, TmuxError> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut escaped = false;
    for &byte in field {
        if escaped {
            decoded.push(byte);
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else {
            decoded.push(byte);
        }
    }
    if escaped {
        return Err(TmuxError::Protocol(format!("dangling {label} escape")));
    }
    utf8(&decoded, label)
}

fn choose_linked_pane<'a>(panes: &[&'a Pane], origin_session: Option<&str>) -> Option<&'a Pane> {
    panes.iter().copied().min_by(|left, right| {
        let left_origin = origin_session.is_some_and(|origin| left.session_id == origin);
        let right_origin = origin_session.is_some_and(|origin| right.session_id == origin);
        right_origin
            .cmp(&left_origin)
            .then_with(|| {
                stable_id_number(&left.session_id, '$')
                    .cmp(&stable_id_number(&right.session_id, '$'))
            })
            .then_with(|| left.target().cmp(&right.target()))
    })
}

fn stable_id(bytes: &[u8], prefix: char, label: &str) -> Result<String, TmuxError> {
    let value = utf8(bytes, label)?;
    if stable_id_number(&value, prefix).is_none() {
        return Err(TmuxError::Protocol(format!("invalid {label}")));
    }
    Ok(value)
}

fn stable_id_number(value: &str, prefix: char) -> Option<u64> {
    let number = value.strip_prefix(prefix)?;
    if !is_decimal(number) {
        return None;
    }
    number.parse().ok()
}

fn decimal_u32(bytes: &[u8], label: &str) -> Result<u32, TmuxError> {
    let value = std::str::from_utf8(bytes)
        .ok()
        .filter(|value| is_decimal(value))
        .and_then(|value| value.parse().ok());
    value.ok_or_else(|| TmuxError::Protocol(format!("invalid {label}")))
}

fn decimal_u64(bytes: &[u8], label: &str) -> Result<u64, TmuxError> {
    let value = std::str::from_utf8(bytes)
        .ok()
        .filter(|value| is_decimal(value))
        .and_then(|value| value.parse().ok());
    value.ok_or_else(|| TmuxError::Protocol(format!("invalid {label}")))
}

fn is_decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn utf8(bytes: &[u8], label: &str) -> Result<String, TmuxError> {
    String::from_utf8(bytes.to_vec())
        .map_err(|_| TmuxError::Protocol(format!("{label} is not UTF-8")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(session: &str, pane_id: &str, pane_pid: u32, dead: bool) -> Pane {
        Pane {
            session_id: session.to_owned(),
            window_id: "@1".to_owned(),
            pane_id: pane_id.to_owned(),
            pane_pid,
            window_index: 1,
            pane_index: 0,
            height: 24,
            width: 80,
            dead,
            last_output: Some(10),
            session_name: "session".to_owned(),
            window_name: "window".to_owned(),
            path: PathBuf::from("/work"),
        }
    }

    #[test]
    fn inventory_decodes_escaped_names_and_missing_timestamp() {
        let payload =
            b"$2\t@4\t%7\t123\t1\t0\t24\t80\t0\t\tteam\\\tname\twin\\\nname\t/work\\ path\n";
        let panes = parse_inventory(payload).unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].session_name, "team\tname");
        assert_eq!(panes[0].window_name, "win\nname");
        assert_eq!(panes[0].path, PathBuf::from("/work path"));
        assert_eq!(panes[0].last_output, None);
        assert_eq!(panes[0].target(), "$2:@4.%7");
    }

    #[test]
    fn inventory_rejects_dangling_quoted_newline() {
        let payload = b"$2\t@4\t%7\t123\t1\t0\t24\t80\t0\t0\tname\twindow\t/path\\";
        assert!(parse_inventory(payload).is_err());
    }

    #[test]
    fn control_framing_keeps_percent_payload_and_rejects_incomplete() {
        let framed = b"%begin 1 2 0\n%not-a-notification\n%end 1 2 0\n";
        assert_eq!(parse_control(framed).unwrap(), b"%not-a-notification");
        assert!(parse_control(b"%begin 1 2 0\ntext\n").is_err());
        assert!(parse_control(b"%begin 1 2 0\n%end 1 2 0").is_err());
        let error = parse_control(b"%begin 1 2 0\nno server\n%error 1 2 0\n").unwrap_err();
        assert_eq!(error.to_string(), "tmux: no server");
    }

    #[test]
    fn control_framing_accepts_an_empty_command_result() {
        assert_eq!(parse_control(b"%begin 1 2 0\n%end 1 2 0\n").unwrap(), b"");
    }

    #[test]
    fn capture_decodes_octal_and_sanitizes_controls() {
        let captured = decode_capture(b"hello\\040world\\033[31m\nnext").unwrap();
        assert_eq!(captured, "hello world\u{fffd}[31m\nnext");
        assert!(decode_capture(b"\\777").is_err());
    }

    #[test]
    fn capture_is_bounded_by_recent_lines_and_safe_utf8_bytes() {
        let mut payload = Vec::new();
        for line in 0..201 {
            payload.extend_from_slice(format!("line-{line}\n").as_bytes());
        }
        let captured = decode_capture(&payload).unwrap();
        assert_eq!(captured.lines().count(), MAX_CAPTURE_LINES);
        assert!(captured.starts_with("line-1\n"));

        let oversized = vec![b'x'; MAX_CAPTURE_BYTES + 1];
        let captured = decode_capture(&oversized).unwrap();
        assert_eq!(captured.len(), MAX_CAPTURE_BYTES);
        assert!(captured.is_char_boundary(captured.len()));
    }

    #[test]
    fn mapping_uses_nearest_live_ancestor_and_origin_link() {
        let mut parents = HashMap::new();
        parents.insert(30, 20);
        parents.insert(20, 10);
        let panes = vec![
            pane("$1", "%5", 10, false),
            pane("$2", "%5", 10, false),
            pane("$1", "%6", 20, false),
        ];
        let mapped = map_agent(&[30], &parents, &panes, Some("$2")).unwrap();
        assert_eq!(mapped.pane_id, "%6");

        let mut linked_parents = HashMap::new();
        linked_parents.insert(30, 10);
        let mapped = map_agent(&[30], &linked_parents, &panes, Some("$2")).unwrap();
        assert_eq!(mapped.session_id, "$2");
    }

    #[test]
    fn mapping_stops_for_missing_parent_and_cycle() {
        let panes = vec![pane("$1", "%5", 10, false)];
        let mut missing = HashMap::new();
        missing.insert(30, 20);
        assert_eq!(map_agent(&[30], &missing, &panes, None), None);

        let mut cycle = HashMap::new();
        cycle.insert(30, 20);
        cycle.insert(20, 30);
        assert_eq!(map_agent(&[30], &cycle, &panes, None), None);
    }

    #[test]
    fn dead_panes_never_supply_a_target() {
        let panes = vec![pane("$1", "%5", 10, true)];
        assert_eq!(map_agent(&[10], &HashMap::new(), &panes, None), None);
    }

    #[test]
    fn mapping_does_not_keep_a_disappeared_or_reused_pane_target() {
        let original = vec![pane("$1", "%5", 10, false)];
        assert_eq!(
            map_agent(&[10], &HashMap::new(), &original, None)
                .unwrap()
                .pane_id,
            "%5"
        );
        assert_eq!(map_agent(&[10], &HashMap::new(), &[], None), None);

        let replacement = vec![pane("$1", "%9", 10, false)];
        assert_eq!(
            map_agent(&[10], &HashMap::new(), &replacement, None)
                .unwrap()
                .pane_id,
            "%9"
        );
    }

    #[test]
    fn origin_session_requires_well_formed_right_hand_fields() {
        assert_eq!(origin_session("/tmp/socket,123,7"), Some("$7".to_owned()));
        assert_eq!(
            origin_session("/tmp/a,b/socket,123,7"),
            Some("$7".to_owned())
        );
        assert_eq!(origin_session("/tmp/socket,abc,7"), None);
        assert_eq!(origin_session("/tmp/socket,123,$7"), None);
    }

    #[test]
    fn clients_require_stable_ids() {
        let clients = parse_clients(b"/dev/ttys001\t$1\t%2").unwrap();
        assert_eq!(clients[0].name, "/dev/ttys001");
        assert!(parse_clients(b"/dev/ttys001\t1\t%2").is_err());
    }
}
