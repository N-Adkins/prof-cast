//! Best-effort symbolization of instruction pointers.
//!
//! An address is located in the target's `/proc/<pid>/maps`, converted to a
//! file offset and then to a virtual address via the binary's `PT_LOAD`
//! segments, and finally matched against the ELF symbol table. Anything that
//! cannot be resolved degrades gracefully to the owning module plus the raw
//! address, which is still a valid (if coarse) frame.
#![allow(clippy::arithmetic_side_effects)]

use std::collections::HashMap;
use std::fs;

use object::{Object, ObjectSegment, ObjectSymbol, SymbolKind};
use profcast_core::model::Frame;

/// One executable, file-backed region from `/proc/<pid>/maps`.
struct MapEntry {
    start: u64,
    end: u64,
    file_offset: u64,
    path: String,
}

/// A function symbol: its virtual address, size (0 if unknown), and name.
struct FuncSym {
    vaddr: u64,
    size: u64,
    name: String,
}

/// Parsed ELF data for one binary: function symbols sorted by address, and the
/// `PT_LOAD` segments needed to turn a file offset into a virtual address.
struct Module {
    funcs: Vec<FuncSym>,
    loads: Vec<Load>,
}

/// A `PT_LOAD` segment: maps file offsets `[offset, offset+size)` to vaddrs.
struct Load {
    offset: u64,
    size: u64,
    vaddr: u64,
}

/// Resolves instruction pointers for a single profiled process, caching parsed
/// binaries so each is read at most once.
pub(super) struct Symbolizer {
    maps: Vec<MapEntry>,
    modules: HashMap<String, Option<Module>>,
}

impl Symbolizer {
    /// Builds a symbolizer from the process's memory map. `pid == 0` resolves to
    /// `self`.
    #[must_use]
    pub(super) fn new(pid: u32) -> Self {
        let which = if pid == 0 {
            "self".to_owned()
        } else {
            pid.to_string()
        };
        let maps = fs::read_to_string(format!("/proc/{which}/maps"))
            .map(|text| parse_maps(&text))
            .unwrap_or_default();
        Self {
            maps,
            modules: HashMap::new(),
        }
    }

    /// Turns a raw instruction pointer into a [`Frame`], resolving the function
    /// name and owning module where possible.
    pub(super) fn resolve(&mut self, ip: u64) -> Frame {
        let Some(entry) = self.maps.iter().find(|m| ip >= m.start && ip < m.end) else {
            return Frame {
                raw: format!("0x{ip:x}"),
                address: Some(ip),
                ..Frame::default()
            };
        };

        let module = basename(&entry.path).to_owned();
        let file_offset = ip - entry.start + entry.file_offset;
        let path = entry.path.clone();

        let function =
            Self::module(&mut self.modules, &path).and_then(|m| m.function_at(file_offset));

        let raw = function.clone().unwrap_or_else(|| format!("0x{ip:x}"));

        Frame {
            raw,
            function,
            module: Some(module),
            address: Some(ip),
            ..Frame::default()
        }
    }

    /// Returns the cached [`Module`] for `path`, parsing it on first use.
    fn module<'a>(
        cache: &'a mut HashMap<String, Option<Module>>,
        path: &str,
    ) -> Option<&'a Module> {
        cache
            .entry(path.to_owned())
            .or_insert_with(|| Module::load(path))
            .as_ref()
    }
}

impl Module {
    /// Reads and parses the ELF at `path`, returning `None` if it cannot be
    /// read or parsed (e.g. a `(deleted)` mapping or an unreadable file).
    fn load(path: &str) -> Option<Self> {
        if !path.starts_with('/') || path.ends_with("(deleted)") {
            return None;
        }
        let data = fs::read(path).ok()?;
        let file = object::File::parse(&*data).ok()?;

        let mut loads: Vec<Load> = file
            .segments()
            .filter_map(|seg| {
                let (offset, size) = seg.file_range();
                (size > 0).then_some(Load {
                    offset,
                    size,
                    vaddr: seg.address(),
                })
            })
            .collect();
        loads.sort_by_key(|l| l.offset);

        let mut funcs: Vec<FuncSym> = file
            .symbols()
            .chain(file.dynamic_symbols())
            .filter(|sym| sym.kind() == SymbolKind::Text && sym.address() != 0)
            .filter_map(|sym| {
                let name = sym.name().ok()?;
                (!name.is_empty()).then(|| FuncSym {
                    vaddr: sym.address(),
                    size: sym.size(),
                    name: name.to_owned(),
                })
            })
            .collect();
        funcs.sort_by_key(|f| f.vaddr);

        Some(Self { funcs, loads })
    }

    /// Resolves a file offset to a function name, if one covers it.
    fn function_at(&self, file_offset: u64) -> Option<String> {
        let vaddr = self.file_offset_to_vaddr(file_offset)?;
        // Greatest symbol whose address is <= vaddr.
        let idx = self
            .funcs
            .partition_point(|f| f.vaddr <= vaddr)
            .checked_sub(1)?;
        let sym = self.funcs.get(idx)?;
        // If the symbol has a known size, require the address to fall inside it.
        if sym.size != 0 && vaddr >= sym.vaddr + sym.size {
            return None;
        }
        Some(sym.name.clone())
    }

    /// Maps a file offset into a virtual address via the `PT_LOAD` segments.
    fn file_offset_to_vaddr(&self, file_offset: u64) -> Option<u64> {
        self.loads
            .iter()
            .find(|l| file_offset >= l.offset && file_offset < l.offset + l.size)
            .map(|l| file_offset - l.offset + l.vaddr)
    }
}

/// Parses the executable, file-backed regions out of `/proc/<pid>/maps` text.
fn parse_maps(text: &str) -> Vec<MapEntry> {
    text.lines().filter_map(parse_map_line).collect()
}

/// Parses one `maps` line, keeping only executable regions backed by an
/// absolute file path.
fn parse_map_line(line: &str) -> Option<MapEntry> {
    // Layout: `start-end perms offset dev inode pathname`.
    let mut fields = line
        .splitn(6, char::is_whitespace)
        .filter(|f| !f.is_empty());
    let range = fields.next()?;
    let perms = fields.next()?;
    let offset = fields.next()?;
    let _dev = fields.next()?;
    let _inode = fields.next()?;
    let path = fields.next()?.trim();

    if !perms.contains('x') || !path.starts_with('/') {
        return None;
    }
    let (start, end) = range.split_once('-')?;
    Some(MapEntry {
        start: u64::from_str_radix(start, 16).ok()?,
        end: u64::from_str_radix(end, 16).ok()?,
        file_offset: u64::from_str_radix(offset, 16).ok()?,
        path: path.to_owned(),
    })
}

/// The final path component of `path`, or the whole string if it has none.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_executable_file_backed_line() {
        let line = "5588f0a00000-5588f0a01000 r-xp 00002000 08:01 1311 /usr/bin/cat";
        let entry = parse_map_line(line).expect("should parse");
        assert_eq!(entry.start, 0x5588_f0a0_0000);
        assert_eq!(entry.end, 0x5588_f0a0_1000);
        assert_eq!(entry.file_offset, 0x2000);
        assert_eq!(entry.path, "/usr/bin/cat");
    }

    #[test]
    fn skips_non_executable_and_anonymous() {
        // Non-executable mapping.
        assert!(
            parse_map_line("5588f0a00000-5588f0a01000 rw-p 0 08:01 1311 /usr/bin/cat").is_none()
        );
        // Executable but anonymous / special.
        assert!(parse_map_line("7fff000000-7fff001000 r-xp 0 00:00 0 [vdso]").is_none());
        // Garbage.
        assert!(parse_map_line("not a maps line").is_none());
    }

    #[test]
    fn basename_takes_last_component() {
        assert_eq!(basename("/usr/lib/libc.so.6"), "libc.so.6");
        assert_eq!(basename("bare"), "bare");
    }

    #[test]
    fn file_offset_maps_through_load_segment() {
        let module = Module {
            funcs: vec![
                FuncSym {
                    vaddr: 0x1000,
                    size: 0x10,
                    name: "early".to_owned(),
                },
                FuncSym {
                    vaddr: 0x1100,
                    size: 0,
                    name: "late".to_owned(),
                },
            ],
            loads: vec![Load {
                offset: 0x1000,
                size: 0x1000,
                vaddr: 0x1000,
            }],
        };
        // 0x1108 -> vaddr 0x1108, covered by the size-less "late" symbol.
        assert_eq!(module.function_at(0x1108).as_deref(), Some("late"));
        // Inside "early" (size 0x10).
        assert_eq!(module.function_at(0x1004).as_deref(), Some("early"));
        // Past the end of "early" but before "late": "early"'s size rules it
        // out and nothing else covers the gap.
        assert_eq!(module.function_at(0x1050), None);
        // Offset outside any load segment.
        assert_eq!(module.function_at(0x9000), None);
    }
}
