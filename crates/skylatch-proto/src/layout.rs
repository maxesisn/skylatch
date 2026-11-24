//! Persistent workspace layout: rooms, furniture and device placements.
//!
//! The hub stores one [`WorkspaceLayout`] per account so every client renders the same
//! arranged workstation. All fields added after version 1 must be `#[serde(default)]`.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MAX_ROOMS: usize = 32;
pub const MAX_FURNITURE_PER_ROOM: usize = 32;
pub const MAX_PLACEMENTS: usize = 512;
pub const MAX_ID_LEN: usize = 128;
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_OFFSET: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FurnitureKind {
    LDesk,
    StraightDesk,
    VmRack,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Facing {
    #[default]
    FrontLeft,
    FrontRight,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FurnitureItem {
    pub id: String,
    pub kind: FurnitureKind,
    pub grid_x: i32,
    pub grid_y: i32,
    #[serde(default)]
    pub facing: Facing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoomLayout {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub furniture: Vec<FurnitureItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DevicePlacement {
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    pub room_id: String,
    pub furniture_id: String,
    pub slot: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub du: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dv: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceLayout {
    #[serde(default = "default_layout_version")]
    pub version: u32,
    #[serde(default)]
    pub rooms: Vec<RoomLayout>,
    #[serde(default)]
    pub placements: Vec<DevicePlacement>,
}

fn default_layout_version() -> u32 {
    1
}

impl Default for WorkspaceLayout {
    fn default() -> Self {
        Self {
            version: default_layout_version(),
            rooms: Vec::new(),
            placements: Vec::new(),
        }
    }
}

fn check_id(what: &str, id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!("{what} must not be empty"));
    }
    if id.chars().count() > MAX_ID_LEN {
        return Err(format!("{what} exceeds {MAX_ID_LEN} characters"));
    }
    Ok(())
}

fn check_name(what: &str, name: &str) -> Result<(), String> {
    if name.chars().count() > MAX_NAME_LEN {
        return Err(format!("{what} exceeds {MAX_NAME_LEN} characters"));
    }
    Ok(())
}

fn check_offset(what: &str, value: Option<f32>) -> Result<(), String> {
    match value {
        Some(v) if !v.is_finite() || !(-MAX_OFFSET..=MAX_OFFSET).contains(&v) => {
            Err(format!("{what} must be finite and within -2.0..=2.0"))
        }
        _ => Ok(()),
    }
}

impl WorkspaceLayout {
    /// Checks size caps, identifier rules and value ranges.
    pub fn validate(&self) -> Result<(), String> {
        if self.rooms.len() > MAX_ROOMS {
            return Err(format!("at most {MAX_ROOMS} rooms are allowed"));
        }
        if self.placements.len() > MAX_PLACEMENTS {
            return Err(format!("at most {MAX_PLACEMENTS} placements are allowed"));
        }
        let mut room_ids = HashSet::new();
        for room in &self.rooms {
            check_id("room id", &room.id)?;
            check_name("room name", &room.name)?;
            if !room_ids.insert(room.id.as_str()) {
                return Err(format!("duplicate room id {:?}", room.id));
            }
            if room.furniture.len() > MAX_FURNITURE_PER_ROOM {
                return Err(format!(
                    "at most {MAX_FURNITURE_PER_ROOM} furniture items per room are allowed"
                ));
            }
            let mut furniture_ids = HashSet::new();
            for item in &room.furniture {
                check_id("furniture id", &item.id)?;
                if !furniture_ids.insert(item.id.as_str()) {
                    return Err(format!(
                        "duplicate furniture id {:?} in room {:?}",
                        item.id, room.id
                    ));
                }
            }
        }
        for placement in &self.placements {
            check_id("device id", &placement.device_id)?;
            check_id("placement room id", &placement.room_id)?;
            check_id("placement furniture id", &placement.furniture_id)?;
            if let Some(name) = &placement.custom_name {
                check_name("custom name", name)?;
            }
            check_offset("du", placement.du)?;
            check_offset("dv", placement.dv)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(id: &str, furniture: &[&str]) -> RoomLayout {
        RoomLayout {
            id: id.into(),
            name: "Office".into(),
            furniture: furniture
                .iter()
                .map(|f| FurnitureItem {
                    id: (*f).into(),
                    kind: FurnitureKind::LDesk,
                    grid_x: -1,
                    grid_y: 2,
                    facing: Facing::FrontRight,
                })
                .collect(),
        }
    }

    fn placement(device: &str) -> DevicePlacement {
        DevicePlacement {
            device_id: device.into(),
            custom_name: Some("Bench".into()),
            room_id: "r1".into(),
            furniture_id: "f1".into(),
            slot: 1,
            du: Some(0.5),
            dv: None,
        }
    }

    #[test]
    fn round_trip_and_wire_names() {
        let layout = WorkspaceLayout {
            version: 1,
            rooms: vec![room("r1", &["f1"])],
            placements: vec![placement("d1")],
        };
        let json = serde_json::to_string(&layout).unwrap();
        assert!(json.contains("\"l_desk\"") && json.contains("\"front_right\""));
        assert!(!json.contains("dv"));
        assert_eq!(
            serde_json::from_str::<WorkspaceLayout>(&json).unwrap(),
            layout
        );
        assert_eq!(layout.validate(), Ok(()));
    }

    #[test]
    fn missing_fields_take_defaults() {
        let layout: WorkspaceLayout = serde_json::from_str("{}").unwrap();
        assert_eq!(layout, WorkspaceLayout::default());
        assert_eq!(layout.version, 1);
        let room: RoomLayout = serde_json::from_str(r#"{"id":"r","name":"n"}"#).unwrap();
        assert!(room.furniture.is_empty());
        let item: FurnitureItem =
            serde_json::from_str(r#"{"id":"f","kind":"vm_rack","grid_x":0,"grid_y":0}"#).unwrap();
        assert_eq!(item.facing, Facing::FrontLeft);
    }

    #[test]
    fn validation_bounds() {
        let ok = |l: &WorkspaceLayout| l.validate().is_ok();
        let mut l = WorkspaceLayout {
            rooms: (0..32).map(|i| room(&format!("r{i}"), &[])).collect(),
            ..Default::default()
        };
        assert!(ok(&l));
        l.rooms.push(room("extra", &[]));
        assert!(!ok(&l));

        let l = WorkspaceLayout {
            rooms: vec![room("r", &["a", "a"])],
            ..Default::default()
        };
        assert!(!ok(&l));
        let l = WorkspaceLayout {
            rooms: vec![room("r", &["a"]), room("r", &[])],
            ..Default::default()
        };
        assert!(!ok(&l));
        let ids: Vec<String> = (0..33).map(|i| format!("f{i}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let l = WorkspaceLayout {
            rooms: vec![room("r", &refs)],
            ..Default::default()
        };
        assert!(!ok(&l));
        let l = WorkspaceLayout {
            rooms: vec![room("r", &refs[..32])],
            ..Default::default()
        };
        assert!(ok(&l));

        let l = WorkspaceLayout {
            rooms: vec![room("", &[])],
            ..Default::default()
        };
        assert!(!ok(&l));
        let l = WorkspaceLayout {
            rooms: vec![room(&"x".repeat(129), &[])],
            ..Default::default()
        };
        assert!(!ok(&l));
        let mut r = room("r", &[]);
        r.name = "n".repeat(65);
        let l = WorkspaceLayout {
            rooms: vec![r],
            ..Default::default()
        };
        assert!(!ok(&l));
    }

    #[test]
    fn placement_bounds() {
        let with = |p: DevicePlacement| WorkspaceLayout {
            placements: vec![p],
            ..Default::default()
        };
        assert!(with(placement("d")).validate().is_ok());
        assert!(with(placement("")).validate().is_err());
        let mut p = placement("d");
        p.du = Some(2.0);
        p.dv = Some(-2.0);
        assert!(with(p.clone()).validate().is_ok());
        p.dv = Some(2.01);
        assert!(with(p.clone()).validate().is_err());
        p.dv = Some(f32::NAN);
        assert!(with(p.clone()).validate().is_err());
        p.dv = Some(f32::INFINITY);
        assert!(with(p).validate().is_err());
        let mut p = placement("d");
        p.custom_name = Some("n".repeat(65));
        assert!(with(p).validate().is_err());
        let l = WorkspaceLayout {
            placements: (0..513).map(|i| placement(&format!("d{i}"))).collect(),
            ..Default::default()
        };
        assert!(l.validate().is_err());
    }
}
