//! Small shared helpers for our typed DBC catalog loaders (creatures, game objects).
//!
//! DBC files carry no column types, so each loader supplies a [`Schema`] (field count must match the
//! file header) and reads fields by index. Layouts are verified clean-room against build 5875 (the
//! file header's `fieldCount × 4 == recordSize`, cross-checked with wowdev.wiki).

use std::collections::HashMap;
use std::io::Cursor;

use anyhow::{anyhow, Context, Result};
use benilla_dbc::{DbcParser, FieldType, Record, RecordSet, Schema, SchemaField, StringRef, Value};

use crate::Chain;

/// Parse a DBC's bytes with `schema` into an owned record set (`what` names it for error messages).
pub(crate) fn parse(bytes: &[u8], schema: Schema, what: &str) -> Result<RecordSet> {
    let parser = DbcParser::parse(&mut Cursor::new(bytes))
        .map_err(|e| anyhow!("parsing {what} header: {e}"))?;
    let parser = parser
        .with_schema(schema)
        .map_err(|e| anyhow!("applying {what} schema (field-count mismatch?): {e}"))?;
    parser
        .parse_records()
        .map_err(|e| anyhow!("parsing {what} records: {e}"))
}

/// An unsigned field (accepts the int variants).
pub(crate) fn u32_at(r: &Record, i: usize) -> Option<u32> {
    match r.get_value(i)? {
        Value::UInt32(v) => Some(*v),
        Value::Int32(v) => Some(*v as u32),
        _ => None,
    }
}

/// A float field.
pub(crate) fn f32_at(r: &Record, i: usize) -> Option<f32> {
    match r.get_value(i)? {
        Value::Float32(v) => Some(*v),
        _ => None,
    }
}

/// A signed field (accepts either int variant, mirroring [`u32_at`]'s leniency: the schema tag
/// just picks how the parser decodes the same 4 raw bytes, so a column read as `UInt32` still
/// round-trips through here bit-for-bit).
pub(crate) fn i32_at(r: &Record, i: usize) -> Option<i32> {
    match r.get_value(i)? {
        Value::Int32(v) => Some(*v),
        Value::UInt32(v) => Some(*v as i32),
        _ => None,
    }
}

/// A non-empty string field, resolved through the record set's string block.
pub(crate) fn str_at(rs: &RecordSet, r: &Record, i: usize) -> Option<String> {
    match r.get_value(i)? {
        Value::StringRef(StringRef(off)) => {
            let s = rs.get_string(StringRef(*off)).ok()?;
            (!s.is_empty()).then(|| s.to_string())
        }
        _ => None,
    }
}

/// `SpellIcon.dbc`: id → texture path (`Interface\Icons\…`, extensionless). 1033 records × 2
/// fields (`ID(0)`, `TextureFilename(1, str)`) — verified against build 5875 (`spells.rs`'s own
/// module doc). Shared by [`crate::spells`] (the action-bar catalog) and [`crate::skill_lines`]
/// (a skill line's own tab icon) — one load, not two, since both just want id → path.
pub(crate) fn load_spell_icon_map(chain: &mut Chain) -> Result<HashMap<u32, String>> {
    let bytes = chain
        .read_file("DBFilesClient\\SpellIcon.dbc")
        .context("reading SpellIcon.dbc")?;
    let mut schema = Schema::new("SpellIcon");
    schema.add_field(SchemaField::new("ID", FieldType::UInt32));
    schema.add_field(SchemaField::new("TextureFilename", FieldType::String));
    let set = parse(&bytes, schema, "SpellIcon.dbc")?;
    let mut icons = HashMap::new();
    for r in set.records() {
        if let (Some(id), Some(path)) = (u32_at(r, 0), str_at(&set, r, 1)) {
            icons.insert(id, path);
        }
    }
    Ok(icons)
}

/// A minimal `ID(0) … Name(name_col) …` schema of `cols` u32-wide columns — the shape every
/// "id → localized name" DBC has, since a string column is itself one u32 offset into the string
/// block. Enough for the id→name reads; the loc block's other 7 slots and its flag mask stay
/// anonymous `cN` columns.
pub(crate) fn id_name_schema(what: &str, name_col: usize, cols: usize) -> Schema {
    let mut s = Schema::new(what);
    for i in 0..cols {
        if i == name_col {
            s.add_field(SchemaField::new("Name", FieldType::String));
        } else {
            s.add_field(SchemaField::new(format!("c{i}"), FieldType::UInt32));
        }
    }
    s
}

/// Read an `ID → name` DBC through the patch chain into a map, skipping rows whose name is empty.
/// Shared by every such table ([`crate::quest_headers`]'s AreaTable/QuestSort,
/// [`crate::quest_info`]'s QuestInfo) — the layout differences are the two column numbers.
pub(crate) fn load_id_name_table(
    chain: &mut Chain,
    file: &str,
    name_col: usize,
    cols: usize,
    what: &str,
) -> Result<HashMap<u32, String>> {
    let bytes = chain
        .read_file(file)
        .with_context(|| format!("reading {file}"))?;
    let rs = parse(&bytes, id_name_schema(what, name_col, cols), what)?;
    let mut out = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        // `str_at` already drops an empty string — a row whose name slot points at the string
        // block's leading NUL is a hole, not a name.
        if let (Some(id), Some(name)) = (u32_at(r, 0), str_at(&rs, r, name_col)) {
            out.insert(id, name);
        }
    }
    Ok(out)
}
