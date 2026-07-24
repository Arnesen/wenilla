//! Low-level wire read/write helpers shared by the auth ([`crate::auth`]) and world
//! ([`crate::world`]) protocols. Everything is little endian unless noted; these mirror the exact
//! field encodings the 1.12 client uses.

use std::io::{self, Read, Write};

/// WoW packs an 8-byte GUID as a one-byte mask (which of the 8 bytes are non-zero) followed by only
/// those non-zero bytes, low to high — used throughout the world protocol for object references.
pub fn read_packed_guid(r: &mut impl Read) -> io::Result<u64> {
    let mask = read_u8(r)?;
    let mut guid = 0u64;
    for i in 0..8 {
        if mask & (1 << i) != 0 {
            guid |= (u64::from(read_u8(r)?)) << (i * 8);
        }
    }
    Ok(guid)
}

/// Write a packed GUID (see [`read_packed_guid`]).
pub fn write_packed_guid(guid: u64, w: &mut impl Write) -> io::Result<()> {
    let bytes = guid.to_le_bytes();
    let mut mask = 0u8;
    let mut out = [0u8; 9];
    let mut idx = 1;
    for (i, &b) in bytes.iter().enumerate() {
        if b != 0 {
            mask |= 1 << i;
            out[idx] = b;
            idx += 1;
        }
    }
    out[0] = mask;
    w.write_all(&out[..idx])
}

pub fn read_u8(r: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

pub fn read_u16_le(r: &mut impl Read) -> io::Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

pub fn read_u32_le(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

pub fn read_u32_be(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

pub fn read_i32_le(r: &mut impl Read) -> io::Result<i32> {
    Ok(read_u32_le(r)? as i32)
}

pub fn read_u64_le(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

pub fn read_f32_le(r: &mut impl Read) -> io::Result<f32> {
    Ok(f32::from_bits(read_u32_le(r)?))
}

/// Read a fixed-length byte array.
pub fn read_array<const N: usize>(r: &mut impl Read) -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}

/// Read a NUL-terminated string (the NUL is consumed). Invalid UTF-8 is lossily replaced.
pub fn read_cstring(r: &mut impl Read) -> io::Result<String> {
    let mut bytes = Vec::new();
    loop {
        let b = read_u8(r)?;
        if b == 0 {
            break;
        }
        bytes.push(b);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// A 3-float position/vector in raw WoW coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector3d {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vector3d {
    pub fn read(r: &mut impl Read) -> io::Result<Self> {
        Ok(Self {
            x: read_f32_le(r)?,
            y: read_f32_le(r)?,
            z: read_f32_le(r)?,
        })
    }

    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&self.x.to_le_bytes())?;
        w.write_all(&self.y.to_le_bytes())?;
        w.write_all(&self.z.to_le_bytes())
    }
}

/// Decode a movement-spline packed point (`SMSG_MONSTER_MOVE` packs every point after the first this
/// way): **signed** two's-complement fields — 11 bits x, 11 bits y, 10 bits z — in quarter-yard
/// units (the server packs `round(v * 4)` per axis; vmangos `ByteBuffer::appendPackXYZ`). The
/// decoded vector is the offset from the spline's **destination** back to this waypoint
/// (`destination - waypoint`; vmangos `PacketBuilder::WriteLinearPath`), not an absolute position.
pub fn packed_to_vector3d(p: i32) -> Vector3d {
    // Sign-extend each field: shift it to the top of the i32, then arithmetic-shift back down.
    Vector3d {
        x: ((p << 21) >> 21) as f32 * 0.25,
        y: ((p << 10) >> 21) as f32 * 0.25,
        z: (p >> 22) as f32 * 0.25,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The producer's encoder, transcribed from vmangos `ByteBuffer::appendPackXYZ`:
    /// `packed |= ((int)lroundf(v * 4.0f) & mask) << shift` with masks 0x7FF/0x7FF/0x3FF.
    fn pack_xyz(x: f32, y: f32, z: f32) -> i32 {
        let mut packed = 0u32;
        packed |= ((x * 4.0).round() as i32 & 0x7FF) as u32;
        packed |= (((y * 4.0).round() as i32 & 0x7FF) as u32) << 11;
        packed |= (((z * 4.0).round() as i32 & 0x3FF) as u32) << 22;
        packed as i32
    }

    #[test]
    fn packed_point_roundtrips_signed_quarter_yards() {
        // Field ranges: x/y are 11-bit signed (−256 .. 255.75 yd), z 10-bit signed (−128 .. 127.75 yd).
        // Quarter-yard multiples are exact in f32, so the round-trip must be exact — including the
        // negative offsets the old unsigned decode destroyed.
        for &(x, y, z) in &[
            (0.0f32, 0.0f32, 0.0f32),
            (1.25, -2.5, 3.75),
            (-100.25, 100.0, -50.5),
            (255.75, -256.0, 127.75),
            (-0.25, 0.25, -0.25),
        ] {
            let v = packed_to_vector3d(pack_xyz(x, y, z));
            assert_eq!((v.x, v.y, v.z), (x, y, z));
        }
    }
}
