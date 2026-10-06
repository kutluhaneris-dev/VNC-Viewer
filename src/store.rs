//! Saved connections, persisted locally by eframe (no account, no cloud).

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SavedConnection {
    pub id: u64,
    pub name: String,
    pub address: String,
    /// Only kept when the user ticks "remember password". Stored in the
    /// local settings file as plain text (VNC passwords are weak anyway).
    pub password: Option<String>,
    pub view_only: bool,
    /// Unix seconds of the last successful connection.
    pub last_used: Option<u64>,
}

impl SavedConnection {
    pub fn title(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.address
        } else {
            &self.name
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    pub connections: Vec<SavedConnection>,
    next_id: u64,
    /// Changed since the last write to disk.
    #[serde(skip)]
    pub dirty: bool,
}

impl Store {
    pub const KEY: &str = "connections";

    pub fn upsert(&mut self, mut conn: SavedConnection) -> u64 {
        self.dirty = true;
        if let Some(existing) = self
            .connections
            .iter_mut()
            .find(|c| c.id == conn.id && conn.id != 0)
        {
            *existing = conn;
            return existing.id;
        }
        self.next_id += 1;
        conn.id = self.next_id;
        self.connections.push(conn);
        self.next_id
    }

    pub fn remove(&mut self, id: u64) {
        self.dirty = true;
        self.connections.retain(|c| c.id != id);
    }

    pub fn touch(&mut self, id: u64) {
        self.dirty = true;
        if let Some(c) = self.connections.iter_mut().find(|c| c.id == id) {
            c.last_used = Some(now());
        }
    }

    /// Most recently used first, then alphabetical.
    pub fn sorted(&self) -> Vec<&SavedConnection> {
        let mut v: Vec<_> = self.connections.iter().collect();
        v.sort_by(|a, b| {
            b.last_used
                .cmp(&a.last_used)
                .then_with(|| a.title().to_lowercase().cmp(&b.title().to_lowercase()))
        });
        v
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "az önce", "5 dk önce", "3 gün önce" ...
pub fn ago(ts: u64) -> String {
    let d = now().saturating_sub(ts);
    match d {
        0..60 => "az önce".into(),
        60..3600 => format!("{} dk önce", d / 60),
        3600..86400 => format!("{} saat önce", d / 3600),
        _ => format!("{} gün önce", d / 86400),
    }
}

/// Accepts "host", "host:port", "host::port" (TightVNC style) and "[v6]:port".
pub fn normalize_address(input: &str) -> String {
    let a = input.trim();
    if let Some((host, port)) = a.split_once("::")
        && !host.is_empty()
        && !host.contains(':')
        && port.parse::<u16>().is_ok()
    {
        return format!("{host}:{port}");
    }
    let has_port = if a.starts_with('[') {
        a.contains("]:")
    } else {
        a.matches(':').count() == 1
    };
    if has_port {
        a.to_string()
    } else if a.contains(':') {
        format!("[{a}]:5900")
    } else {
        format!("{a}:5900")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(normalize_address("pc"), "pc:5900");
        assert_eq!(normalize_address(" 10.0.0.2:5901 "), "10.0.0.2:5901");
        assert_eq!(normalize_address("pc::5902"), "pc:5902");
        assert_eq!(normalize_address("::1"), "[::1]:5900");
        assert_eq!(normalize_address("[::1]:5901"), "[::1]:5901");
    }
}
