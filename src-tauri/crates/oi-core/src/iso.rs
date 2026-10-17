//! Minimal, dependency-free ISO9660 (ECMA-119) image writer with optional Joliet.
//!
//! Two distinct artefacts are produced by this module:
//!
//! 1. The boot-time **seed configuration disk** (`OILinux_PRD.md` §5.3), a read-only optical
//!    image carrying the per-machine SSH public keys, the host SFTP endpoint and the trust
//!    anchors. File names are plain ISO level 1 (`SEED.JSON`, `AUTHORIZED_KEYS`) because both
//!    sides of that contract are ours.
//! 2. The NoCloud **`cidata`** image used while (re)building the Alpine base image. `cloud-init`
//!    requires the hyphenated names `user-data` / `meta-data`, which ISO level 1 cannot express,
//!    so that artefact is written with a Joliet (UCS-2) supplementary volume descriptor while the
//!    file contents stay in one shared data area.
//!
//! Everything here is deterministic for a given input, which keeps image builds reproducible.

use crate::error::{OiError, OiResult};

pub const SECTOR: usize = 2048;

/// A file placed in the image root.
#[derive(Debug, Clone)]
pub struct IsoFile {
    /// Name as the guest should see it. With `joliet = false` this must be a valid 8.3
    /// identifier; with `joliet = true` it may use lower case and dashes.
    pub name: String,
    pub data: Vec<u8>,
}

impl IsoFile {
    pub fn new(name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        IsoFile {
            name: name.into(),
            data: data.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IsoOptions {
    pub volume_id: String,
    /// Write a Joliet supplementary volume descriptor alongside the primary one.
    pub joliet: bool,
    pub unix_secs: u64,
}

impl IsoOptions {
    pub fn new(volume_id: impl Into<String>) -> Self {
        IsoOptions {
            volume_id: volume_id.into(),
            joliet: false,
            unix_secs: DEFAULT_UNIX_SECS,
        }
    }

    pub fn with_joliet(mut self, joliet: bool) -> Self {
        self.joliet = joliet;
        self
    }

    pub fn with_unix_secs(mut self, secs: u64) -> Self {
        self.unix_secs = secs;
        self
    }
}

/// Deterministic default timestamp (2026-10-16 00:00:00 UTC) used when none is supplied.
const DEFAULT_UNIX_SECS: u64 = 1_792_108_800;

const PVD_SECTOR: u32 = 16;
const SVD_SECTOR: u32 = 17;
const TERM_SECTOR: u32 = 18;
const ISO_L_PATH_SECTOR: u32 = 19;
const ISO_M_PATH_SECTOR: u32 = 20;
const JOLIET_L_PATH_SECTOR: u32 = 21;
const JOLIET_M_PATH_SECTOR: u32 = 22;
const ISO_ROOT_SECTOR: u32 = 23;

/// Build a complete ISO9660 image in memory with default options.
pub fn build_iso9660(volume_id: &str, files: &[IsoFile]) -> OiResult<Vec<u8>> {
    build_iso(IsoOptions::new(volume_id), files)
}

pub fn build_iso(opts: IsoOptions, files: &[IsoFile]) -> OiResult<Vec<u8>> {
    let vol_iso = normalize_volume_id(&opts.volume_id)?;
    let date = iso_date_short(opts.unix_secs);
    let long_date = iso_date_long(opts.unix_secs);

    // --- validate + sort identifiers ------------------------------------------------
    let mut ids: Vec<(String, String, &IsoFile)> = Vec::with_capacity(files.len());
    for f in files {
        let iso_id = match validate_short_name(&f.name) {
            Ok(id) => id,
            // A name that only exists in the supplementary (Joliet) volume still needs a primary
            // identifier, because the file data is shared by both volumes. Callers that do not
            // write a Joliet volume keep the strict behaviour.
            Err(_) if opts.joliet => joliet_fallback_id(&f.name)?,
            Err(e) => return Err(e),
        };
        let joliet_name = if opts.joliet {
            validate_joliet_name(&f.name)?
        } else {
            String::new()
        };
        ids.push((iso_id, joliet_name, f));
    }
    ids.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for w in ids.windows(2) {
        if w[0].0 == w[1].0 {
            return Err(OiError::other(format!(
                "duplicate ISO file identifier: {}",
                w[0].0
            )));
        }
    }

    // --- sector allocation -----------------------------------------------------------
    let iso_root_records_len = 2 * dir_record_len(&[0x00])
        + ids
            .iter()
            .map(|(id, _, _)| dir_record_len(id.as_bytes()))
            .sum::<usize>();
    let iso_root_sectors = (iso_root_records_len.div_ceil(SECTOR) as u32).max(1);
    let iso_root_bytes = iso_root_sectors as usize * SECTOR;

    let mut cursor = ISO_ROOT_SECTOR + iso_root_sectors;

    let joliet_root_sector = if opts.joliet {
        let joliet_root_records_len = 2 * dir_record_len(&[0x00])
            + ids
                .iter()
                .map(|(_, jn, _)| dir_record_len(&joliet_name_bytes(jn)))
                .sum::<usize>();
        let joliet_root_sectors = (joliet_root_records_len.div_ceil(SECTOR) as u32).max(1);
        let sector = cursor;
        cursor += joliet_root_sectors;
        Some((sector, joliet_root_sectors as usize * SECTOR))
    } else {
        None
    };

    let mut file_lbas: Vec<u32> = Vec::with_capacity(ids.len());
    for (_, _, f) in &ids {
        file_lbas.push(cursor);
        cursor += (f.data.len().div_ceil(SECTOR) as u32).max(1);
    }
    let total_sectors = cursor;

    let mut img = vec![0u8; total_sectors as usize * SECTOR];

    // --- Primary Volume Descriptor ---------------------------------------------------
    {
        let pvd = &mut img[PVD_SECTOR as usize * SECTOR..(PVD_SECTOR as usize + 1) * SECTOR];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        pvd[8..40].copy_from_slice(&pad_spaces("OI-LINUX-SEED", 32));
        pvd[40..72].copy_from_slice(&vol_iso);
        both_u32(&mut pvd[80..88], total_sectors);
        both_u16(&mut pvd[120..124], 1);
        both_u16(&mut pvd[124..128], 1);
        both_u16(&mut pvd[128..132], SECTOR as u16);
        both_u32(&mut pvd[132..140], path_table_size() as u32);
        pvd[140..144].copy_from_slice(&ISO_L_PATH_SECTOR.to_le_bytes());
        pvd[148..152].copy_from_slice(&ISO_M_PATH_SECTOR.to_be_bytes());
        let root = dir_record(&[0x00], ISO_ROOT_SECTOR, iso_root_bytes as u32, 0x02, &date);
        pvd[156..156 + root.len()].copy_from_slice(&root);
        pvd[190..318].copy_from_slice(&pad_spaces("OI-LINUX", 128));
        pvd[574..702].copy_from_slice(&pad_spaces("OI-LINUX SEED", 128));
        pvd[813..830].copy_from_slice(&long_date);
        pvd[830..847].copy_from_slice(&long_date);
        pvd[847..864].copy_from_slice(&EMPTY_LONG_DATE);
        pvd[864..881].copy_from_slice(&EMPTY_LONG_DATE);
        pvd[881] = 1;
    }

    // --- Joliet Supplementary Volume Descriptor --------------------------------------
    if let Some((joliet_root_sector, joliet_root_bytes)) = joliet_root_sector {
        let svd = &mut img[SVD_SECTOR as usize * SECTOR..(SVD_SECTOR as usize + 1) * SECTOR];
        svd[0] = 2;
        svd[1..6].copy_from_slice(b"CD001");
        svd[6] = 1;
        // UCS-2 Level 3 escape sequence
        svd[88] = 0x25;
        svd[89] = 0x2F;
        svd[90] = 0x45;
        for b in svd[91..120].iter_mut() {
            *b = 0x20;
        }
        svd[8..40].copy_from_slice(&pad_spaces("OI-LINUX-SEED", 32));
        let mut vol_ucs2 = vec![0u8; 32];
        let upper = opts.volume_id.to_ascii_uppercase();
        for (i, ch) in upper.chars().take(16).enumerate() {
            let c = ch as u32;
            vol_ucs2[i * 2] = ((c >> 8) & 0xff) as u8;
            vol_ucs2[i * 2 + 1] = (c & 0xff) as u8;
        }
        for i in upper.chars().count()..16 {
            vol_ucs2[i * 2] = 0x00;
            vol_ucs2[i * 2 + 1] = 0x20;
        }
        svd[40..72].copy_from_slice(&vol_ucs2);
        both_u32(&mut svd[80..88], total_sectors);
        both_u16(&mut svd[120..124], 1);
        both_u16(&mut svd[124..128], 1);
        both_u16(&mut svd[128..132], SECTOR as u16);
        both_u32(&mut svd[132..140], path_table_size() as u32);
        svd[140..144].copy_from_slice(&JOLIET_L_PATH_SECTOR.to_le_bytes());
        svd[148..152].copy_from_slice(&JOLIET_M_PATH_SECTOR.to_be_bytes());
        let root = dir_record(
            &[0x00],
            joliet_root_sector,
            joliet_root_bytes as u32,
            0x02,
            &date,
        );
        svd[156..156 + root.len()].copy_from_slice(&root);
        svd[190..318].copy_from_slice(&pad_spaces("OI-LINUX", 128));
        svd[574..702].copy_from_slice(&pad_spaces("OI-LINUX SEED", 128));
        svd[813..830].copy_from_slice(&long_date);
        svd[830..847].copy_from_slice(&long_date);
        svd[847..864].copy_from_slice(&EMPTY_LONG_DATE);
        svd[864..881].copy_from_slice(&EMPTY_LONG_DATE);
        svd[881] = 1;
    }

    // --- Volume Descriptor Set Terminator --------------------------------------------
    {
        let t = &mut img[TERM_SECTOR as usize * SECTOR..(TERM_SECTOR as usize + 1) * SECTOR];
        t[0] = 255;
        t[1..6].copy_from_slice(b"CD001");
        t[6] = 1;
    }

    // --- Path tables ------------------------------------------------------------------
    write_path_table(&mut img, ISO_L_PATH_SECTOR, ISO_ROOT_SECTOR, true, 1);
    write_path_table(&mut img, ISO_M_PATH_SECTOR, ISO_ROOT_SECTOR, false, 1);
    if let Some((joliet_root_sector, _)) = joliet_root_sector {
        write_path_table(&mut img, JOLIET_L_PATH_SECTOR, joliet_root_sector, true, 1);
        write_path_table(&mut img, JOLIET_M_PATH_SECTOR, joliet_root_sector, false, 1);
    }

    // --- Root directories --------------------------------------------------------------
    {
        let dot = dir_record(&[0x00], ISO_ROOT_SECTOR, iso_root_bytes as u32, 0x02, &date);
        let dotdot = dir_record(&[0x01], ISO_ROOT_SECTOR, iso_root_bytes as u32, 0x02, &date);
        let mut off = ISO_ROOT_SECTOR as usize * SECTOR;
        for r in [&dot, &dotdot] {
            img[off..off + r.len()].copy_from_slice(r);
            off += r.len();
        }
        for (i, (id, _, f)) in ids.iter().enumerate() {
            let rec = dir_record(id.as_bytes(), file_lbas[i], f.data.len() as u32, 0x00, &date);
            img[off..off + rec.len()].copy_from_slice(&rec);
            off += rec.len();
        }
    }

    if let Some((joliet_root_sector, joliet_root_bytes)) = joliet_root_sector {
        let dot = dir_record(
            &[0x00],
            joliet_root_sector,
            joliet_root_bytes as u32,
            0x02,
            &date,
        );
        let dotdot = dir_record(
            &[0x01],
            joliet_root_sector,
            joliet_root_bytes as u32,
            0x02,
            &date,
        );
        let mut off = joliet_root_sector as usize * SECTOR;
        for r in [&dot, &dotdot] {
            img[off..off + r.len()].copy_from_slice(r);
            off += r.len();
        }
        for (i, (_, jn, f)) in ids.iter().enumerate() {
            let rec = dir_record(
                &joliet_name_bytes(jn),
                file_lbas[i],
                f.data.len() as u32,
                0x00,
                &date,
            );
            img[off..off + rec.len()].copy_from_slice(&rec);
            off += rec.len();
        }
    }

    // --- File data ----------------------------------------------------------------------
    for (i, (_, _, f)) in ids.iter().enumerate() {
        let off = file_lbas[i] as usize * SECTOR;
        img[off..off + f.data.len()].copy_from_slice(&f.data);
    }

    Ok(img)
}

const EMPTY_LONG_DATE: [u8; 17] = *b"0000000000000000\0";

fn path_table_size() -> usize {
    (path_table_record_len(&[0x00]).div_ceil(SECTOR) as u32 * SECTOR as u32) as usize
}

fn write_path_table(img: &mut [u8], sector: u32, root_sector: u32, le: bool, parent: u16) {
    let rec = path_table_record(&[0x00], root_sector, parent, le);
    let off = sector as usize * SECTOR;
    img[off..off + rec.len()].copy_from_slice(&rec);
}

fn dir_record_len(ident: &[u8]) -> usize {
    33 + ident.len() + if ident.len() % 2 == 0 { 1 } else { 0 }
}

fn dir_record(ident: &[u8], extent: u32, data_len: u32, flags: u8, date: &[u8; 7]) -> Vec<u8> {
    let len = dir_record_len(ident);
    let mut r = vec![0u8; len];
    r[0] = len as u8;
    both_u32(&mut r[2..10], extent);
    both_u32(&mut r[10..18], data_len);
    r[18..25].copy_from_slice(date);
    r[25] = flags;
    both_u16(&mut r[28..32], 1);
    r[32] = ident.len() as u8;
    r[33..33 + ident.len()].copy_from_slice(ident);
    r
}

fn path_table_record_len(ident: &[u8]) -> usize {
    8 + ident.len() + if ident.len() % 2 == 1 { 1 } else { 0 }
}

fn path_table_record(ident: &[u8], extent: u32, parent: u16, little_endian: bool) -> Vec<u8> {
    let len = path_table_record_len(ident);
    let mut r = vec![0u8; len];
    r[0] = ident.len() as u8;
    if little_endian {
        r[2..6].copy_from_slice(&extent.to_le_bytes());
        r[6..8].copy_from_slice(&parent.to_le_bytes());
    } else {
        r[2..6].copy_from_slice(&extent.to_be_bytes());
        r[6..8].copy_from_slice(&parent.to_be_bytes());
    }
    r[8..8 + ident.len()].copy_from_slice(ident);
    r
}

fn both_u16(dst: &mut [u8], v: u16) {
    dst[0..2].copy_from_slice(&v.to_le_bytes());
    dst[2..4].copy_from_slice(&v.to_be_bytes());
}

fn both_u32(dst: &mut [u8], v: u32) {
    dst[0..4].copy_from_slice(&v.to_le_bytes());
    dst[4..8].copy_from_slice(&v.to_be_bytes());
}

fn pad_spaces(s: &str, n: usize) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.resize(n, b' ');
    v
}

fn joliet_name_bytes(name: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(name.len() * 2);
    for ch in name.chars() {
        let c = ch as u32;
        if c <= 0xFFFF {
            v.push(((c >> 8) & 0xff) as u8);
            v.push((c & 0xff) as u8);
        }
    }
    v
}

fn normalize_volume_id(s: &str) -> OiResult<Vec<u8>> {
    let up = s.to_ascii_uppercase();
    if up.is_empty() || up.len() > 32 {
        return Err(OiError::other(format!(
            "invalid ISO volume identifier (1..=32 chars): {s}"
        )));
    }
    if !up
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(OiError::other(format!(
            "invalid ISO volume identifier (A-Z, 0-9, _ only): {s}"
        )));
    }
    Ok(pad_spaces(&up, 32))
}

/// Derive a legal primary identifier for a file whose real name only exists in the Joliet
/// volume. Non-identifier characters become `_`, and the base/extension lengths are clamped so a
/// name such as `user-data` becomes `USER-DATA` -> `USER_DATA;1`.
fn joliet_fallback_id(name: &str) -> OiResult<String> {
    let up = name.to_ascii_uppercase();
    let (base, ext) = match up.split_once('.') {
        Some((b, e)) if !e.contains('.') => (b.to_string(), e.to_string()),
        _ => (up.clone(), String::new()),
    };
    let sanitise = |s: &str, max: usize| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(max)
            .collect()
    };
    let base = sanitise(&base, 8);
    let ext = sanitise(&ext, 4);
    if base.is_empty() {
        return Err(OiError::other(format!(
            "cannot derive an ISO9660 identifier for {name}"
        )));
    }
    Ok(if ext.is_empty() {
        format!("{base};1")
    } else {
        format!("{base}.{ext};1")
    })
}

/// Validate a name against the restricted identifier form used here: uppercase, base up to 8
/// characters and extension up to 4 (so that `SEED.JSON` and `WINHOSTS` are both legal), with the
/// `;1` version suffix appended. Linux reads this as an ordinary ISO9660 level-2 name.
pub fn validate_short_name(name: &str) -> OiResult<String> {
    let up = name.to_ascii_uppercase();
    let (base, ext) = match up.split_once('.') {
        Some((b, e)) if !e.contains('.') => (b, e),
        Some(_) => {
            return Err(OiError::other(format!(
                "invalid ISO file name (more than one dot): {name}"
            )))
        }
        None => (up.as_str(), ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 4 {
        return Err(OiError::other(format!(
            "invalid ISO file name (base up to 8 characters, extension up to 4): {name}"
        )));
    }
    let ok = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    };
    if !ok(base) || (!ext.is_empty() && !ok(ext)) {
        return Err(OiError::other(format!(
            "invalid ISO file name (A-Z, 0-9, _ only): {name}"
        )));
    }
    Ok(if ext.is_empty() {
        format!("{base};1")
    } else {
        format!("{base}.{ext};1")
    })
}

/// Validate a name against Joliet (UCS-2, up to 32 characters, no separators).
pub fn validate_joliet_name(name: &str) -> OiResult<String> {
    let chars = name.chars().count();
    if name.is_empty() || chars > 32 {
        return Err(OiError::other(format!(
            "invalid Joliet file name (1..=32 characters): {name}"
        )));
    }
    if name.contains(['/', '\\', ';', ':'])
        || name
            .chars()
            .any(|c| (c as u32) < 0x20 || c == '\u{7f}')
    {
        return Err(OiError::other(format!(
            "invalid Joliet file name (separators and control characters are not allowed): {name}"
        )));
    }
    if name.starts_with('.') {
        return Err(OiError::other(format!(
            "invalid Joliet file name (must not start with a dot): {name}"
        )));
    }
    Ok(name.to_string())
}

/// Convert unix seconds to the ISO9660 7-byte "short" date/time form.
fn iso_date_short(unix_secs: u64) -> [u8; 7] {
    let (y, mo, d, h, mi, s) = civil_from_unix(unix_secs);
    [
        (y - 1900) as u8,
        mo as u8,
        d as u8,
        h as u8,
        mi as u8,
        s as u8,
        0,
    ]
}

/// Convert unix seconds to the ISO9660 17-byte "long" date/time form.
fn iso_date_long(unix_secs: u64) -> [u8; 17] {
    let (y, mo, d, h, mi, s) = civil_from_unix(unix_secs);
    let mut v = [0u8; 17];
    v[0..4].copy_from_slice(format!("{y:04}").as_bytes());
    v[4..6].copy_from_slice(format!("{mo:02}").as_bytes());
    v[6..8].copy_from_slice(format!("{d:02}").as_bytes());
    v[8..10].copy_from_slice(format!("{h:02}").as_bytes());
    v[10..12].copy_from_slice(format!("{mi:02}").as_bytes());
    v[12..14].copy_from_slice(format!("{s:02}").as_bytes());
    v[14..16].copy_from_slice(b"00");
    v[16] = 0;
    v
}

/// Days-to-civil conversion (Howard Hinnant's `civil_from_days`).
fn civil_from_unix(unix_secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let secs = unix_secs as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m as u32,
        d as u32,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_record(img: &[u8], root_sector: u32, ident: &[u8]) -> Option<(u32, u32)> {
        let mut off = root_sector as usize * SECTOR;
        loop {
            let len = img[off] as usize;
            if len == 0 {
                // advance to next sector boundary within the directory extent
                let next = (off / SECTOR + 1) * SECTOR;
                if next >= img.len() || img[next] == 0 && next + 1 >= img.len() {
                    return None;
                }
                off = next;
                continue;
            }
            let name_len = img[off + 32] as usize;
            if name_len == ident.len() && &img[off + 33..off + 33 + name_len] == ident {
                let lba = u32::from_le_bytes(img[off + 2..off + 6].try_into().unwrap());
                let size = u32::from_le_bytes(img[off + 10..off + 14].try_into().unwrap());
                return Some((lba, size));
            }
            off += len;
            if off % SECTOR == 0 && img[off] == 0 {
                return None;
            }
        }
    }

    #[test]
    fn short_names_are_normalised() {
        assert_eq!(validate_short_name("seed.json").unwrap(), "SEED.JSON;1");
        assert!(validate_short_name("meta-data").is_err());
        assert_eq!(validate_short_name("authkeys").unwrap(), "AUTHKEYS;1");
        assert_eq!(validate_short_name("abcdefgh.12").unwrap(), "ABCDEFGH.12;1");
        assert!(validate_short_name("abcdefghi").is_err());
        assert!(validate_short_name("a.b.c").is_err());
        assert!(validate_short_name("abcdefgh.jsonx").is_err());
        assert_eq!(validate_short_name("winhosts").unwrap(), "WINHOSTS;1");
    }

    #[test]
    fn joliet_names_accept_dashes() {
        assert_eq!(validate_joliet_name("meta-data").unwrap(), "meta-data");
        assert_eq!(validate_joliet_name("user-data").unwrap(), "user-data");
        assert!(validate_joliet_name("../etc/passwd").is_err());
        assert!(validate_joliet_name("").is_err());
    }

    #[test]
    fn joliet_fallback_identifiers_keep_the_readable_name() {
        // the primary volume can only carry 8.3 identifiers, so longer names are clamped
        assert_eq!(joliet_fallback_id("user-data").unwrap(), "USER_DAT;1");
        assert_eq!(joliet_fallback_id("meta-data").unwrap(), "META_DAT;1");
        assert_eq!(joliet_fallback_id("cloud-config.yaml").unwrap(), "CLOUD_CO.YAML;1");
        // the fallback is only used when a Joliet volume is written
        let files = vec![IsoFile::new("user-data", b"x".to_vec())];
        assert!(build_iso9660("CIDATA", &files).is_err());
        assert!(build_iso(IsoOptions::new("CIDATA").with_joliet(true), &files).is_ok());
    }

    #[test]
    fn primary_volume_descriptor_is_well_formed() {
        let files = vec![
            IsoFile::new("seed.json", b"{\"a\":1}".to_vec()),
            IsoFile::new("authkeys", b"ssh-ed25519 AAA".to_vec()),
        ];
        let img = build_iso9660("OI_SEED", &files).unwrap();
        assert_eq!(img.len() % SECTOR, 0);
        assert!(img.len() > ISO_ROOT_SECTOR as usize * SECTOR);
        assert_eq!(&img[16 * SECTOR + 1..16 * SECTOR + 6], b"CD001");
        assert_eq!(img[16 * SECTOR], 1);
        // The writer always reserves sector 17 for a supplementary descriptor and puts the set
        // terminator after it, whether or not a Joliet volume was requested.
        assert_eq!(img[18 * SECTOR], 255, "terminator follows the descriptor set");
        assert_eq!(&img[16 * SECTOR + 40..16 * SECTOR + 47], b"OI_SEED");
        let le = u32::from_le_bytes(img[16 * SECTOR + 80..16 * SECTOR + 84].try_into().unwrap());
        let be = u32::from_be_bytes(img[16 * SECTOR + 84..16 * SECTOR + 88].try_into().unwrap());
        assert_eq!(le, be);
        assert_eq!(le as usize * SECTOR, img.len());
        let root = &img[16 * SECTOR + 156..16 * SECTOR + 190];
        assert_eq!(root[25], 0x02);
        assert_eq!(
            u32::from_le_bytes(root[2..6].try_into().unwrap()),
            ISO_ROOT_SECTOR
        );

        let (lba, size) = find_record(&img, ISO_ROOT_SECTOR, b"SEED.JSON;1").expect("seed record");
        assert_eq!(size as usize, 7);
        assert_eq!(
            &img[lba as usize * SECTOR..lba as usize * SECTOR + 7],
            b"{\"a\":1}"
        );

        let (lba, size) =
            find_record(&img, ISO_ROOT_SECTOR, b"AUTHKEYS;1").expect("the second file");
        assert_eq!(size as usize, 15);
        assert_eq!(
            &img[lba as usize * SECTOR..lba as usize * SECTOR + 12],
            b"ssh-ed25519 "
        );
    }

    #[test]
    fn joliet_volume_descriptor_is_well_formed() {
        let files = vec![
            IsoFile::new("user-data", b"#cloud-config\n".to_vec()),
            IsoFile::new("meta-data", b"instance-id: oi\n".to_vec()),
        ];
        let img = build_iso(
            IsoOptions::new("CIDATA").with_joliet(true),
            &files,
        )
        .unwrap();
        assert_eq!(img[SVD_SECTOR as usize * SECTOR], 2);
        assert_eq!(
            &img[SVD_SECTOR as usize * SECTOR + 1..SVD_SECTOR as usize * SECTOR + 6],
            b"CD001"
        );
        assert_eq!(
            &img[SVD_SECTOR as usize * SECTOR + 88..SVD_SECTOR as usize * SECTOR + 91],
            &[0x25, 0x2f, 0x45]
        );
        assert_eq!(img[TERM_SECTOR as usize * SECTOR], 255);
        let root = &img[SVD_SECTOR as usize * SECTOR + 156..SVD_SECTOR as usize * SECTOR + 190];
        let root_sector = u32::from_le_bytes(root[2..6].try_into().unwrap());
        assert_eq!(root_sector, ISO_ROOT_SECTOR + 1, "joliet root follows ISO root");
        let (lba, size) =
            find_record(&img, root_sector, &joliet_name_bytes("user-data")).expect("user-data");
        assert_eq!(size as usize, 14);
        assert_eq!(
            &img[lba as usize * SECTOR..lba as usize * SECTOR + 13],
            b"#cloud-config"
        );
    }

    #[test]
    fn image_is_deterministic() {
        let files = vec![IsoFile::new("seed.json", b"hello".to_vec())];
        let a = build_iso9660("OI_SEED", &files).unwrap();
        let b = build_iso9660("OI_SEED", &files).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn duplicate_identifiers_are_rejected() {
        let files = vec![
            IsoFile::new("seed.json", b"a".to_vec()),
            IsoFile::new("SEED.JSON", b"b".to_vec()),
        ];
        assert!(build_iso9660("OI_SEED", &files).is_err());
    }

    #[test]
    fn many_files_span_multiple_directory_sectors() {
        let files: Vec<IsoFile> = (0..60)
            .map(|i| IsoFile::new(format!("F{i:02}.TXT"), vec![b'x'; 10]))
            .collect();
        let img = build_iso9660("OI_SEED", &files).unwrap();
        for i in [0usize, 30, 59] {
            let id = format!("F{i:02}.TXT;1");
            let (lba, size) = find_record(&img, ISO_ROOT_SECTOR, id.as_bytes())
                .unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(size, 10);
            assert_eq!(img[lba as usize * SECTOR], b'x');
        }
    }

    #[test]
    fn civil_conversion_matches_known_dates() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_from_unix(1_792_108_800), (2026, 10, 16, 0, 0, 0));
    }
}
