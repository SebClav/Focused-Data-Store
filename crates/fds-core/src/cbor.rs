//! The part of CBOR (RFC 8949) that FDS version 4 uses, with no dependencies.
//!
//! A record is a CBOR map with text keys. Writers use definite lengths, the
//! shortest form of every head, and map keys sorted by their encoded bytes
//! (RFC 8949 §4.2.1, deterministic encoding), so equal records are equal
//! bytes in any language. Readers accept any valid definite-length item, and
//! can skip a value of any type: that is what lets a reader built for 4.0
//! open a 4.1 file that has fields it has never heard of.
//!
//! CBOR integers inside a head are big-endian, as the RFC says; the FDS file
//! header around them is little-endian (SPEC.md §6).

use crate::FdsError;

/// Nesting deeper than this is refused: a hostile file must not be able to
/// exhaust the stack.
pub const MAX_DEPTH: usize = 32;

fn bad(msg: &str) -> FdsError {
    FdsError::Malformed(msg.to_string())
}

// ── writing ───────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct Writer {
    pub out: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Writer { out: Vec::new() }
    }

    fn head(&mut self, major: u8, n: u64) {
        let m = major << 5;
        if n < 24 {
            self.out.push(m | n as u8);
        } else if n <= 0xFF {
            self.out.push(m | 24);
            self.out.push(n as u8);
        } else if n <= 0xFFFF {
            self.out.push(m | 25);
            self.out.extend_from_slice(&(n as u16).to_be_bytes());
        } else if n <= 0xFFFF_FFFF {
            self.out.push(m | 26);
            self.out.extend_from_slice(&(n as u32).to_be_bytes());
        } else {
            self.out.push(m | 27);
            self.out.extend_from_slice(&n.to_be_bytes());
        }
    }

    pub fn uint(&mut self, n: u64) {
        self.head(0, n);
    }

    pub fn bytes(&mut self, b: &[u8]) {
        self.head(2, b.len() as u64);
        self.out.extend_from_slice(b);
    }

    pub fn text(&mut self, s: &str) {
        self.head(3, s.len() as u64);
        self.out.extend_from_slice(s.as_bytes());
    }

    pub fn array(&mut self, len: usize) {
        self.head(4, len as u64);
    }

    pub fn map(&mut self, len: usize) {
        self.head(5, len as u64);
    }

    pub fn null(&mut self) {
        self.out.push(0xF6);
    }

    /// Appends bytes that are already one encoded item.
    pub fn raw(&mut self, b: &[u8]) {
        self.out.extend_from_slice(b);
    }
}

// ── reading ───────────────────────────────────────────────────────────────

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn is_done(&self) -> bool {
        self.pos == self.buf.len()
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], FdsError> {
        if n > self.remaining() {
            return Err(bad("an item runs past the end of its data"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// Major type and argument of the next item's head.
    fn head(&mut self) -> Result<(u8, u64), FdsError> {
        let b = self.take(1)?[0];
        let (major, info) = (b >> 5, b & 0x1F);
        let arg = match info {
            0..=23 => info as u64,
            24 => self.take(1)?[0] as u64,
            25 => u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as u64,
            26 => u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as u64,
            27 => u64::from_be_bytes(self.take(8)?.try_into().unwrap()),
            31 => return Err(bad("indefinite-length items are not part of FDS")),
            _ => return Err(bad("reserved CBOR additional information")),
        };
        Ok((major, arg))
    }

    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    pub fn uint(&mut self) -> Result<u64, FdsError> {
        match self.head()? {
            (0, n) => Ok(n),
            _ => Err(bad("expected an unsigned integer")),
        }
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], FdsError> {
        match self.head()? {
            (2, n) => self.take(usize::try_from(n).map_err(|_| bad("byte string too long"))?),
            _ => Err(bad("expected a byte string")),
        }
    }

    pub fn text_ref(&mut self) -> Result<&'a str, FdsError> {
        match self.head()? {
            (3, n) => {
                let raw = self.take(usize::try_from(n).map_err(|_| bad("text too long"))?)?;
                std::str::from_utf8(raw).map_err(|_| bad("text is not valid UTF-8"))
            }
            _ => Err(bad("expected text")),
        }
    }

    pub fn text(&mut self) -> Result<String, FdsError> {
        self.text_ref().map(str::to_string)
    }

    /// Text, or CBOR null as `None`.
    pub fn text_or_null(&mut self) -> Result<Option<String>, FdsError> {
        if self.peek() == Some(0xF6) {
            self.pos += 1;
            return Ok(None);
        }
        self.text().map(Some)
    }

    pub fn array_len(&mut self) -> Result<usize, FdsError> {
        match self.head()? {
            // Every element is at least one byte: a count larger than what is
            // left is a lie, and must not become an allocation.
            (4, n) if n as usize <= self.remaining() => Ok(n as usize),
            (4, _) => Err(bad("array longer than its data")),
            _ => Err(bad("expected an array")),
        }
    }

    pub fn map_len(&mut self) -> Result<usize, FdsError> {
        match self.head()? {
            (5, n) if (n as usize).saturating_mul(2) <= self.remaining() => Ok(n as usize),
            (5, _) => Err(bad("map longer than its data")),
            _ => Err(bad("expected a map")),
        }
    }

    /// Steps over one item of any type, without allocating.
    pub fn skip(&mut self) -> Result<(), FdsError> {
        self.skip_at(0)
    }

    fn skip_at(&mut self, depth: usize) -> Result<(), FdsError> {
        if depth > MAX_DEPTH {
            return Err(bad("nesting too deep"));
        }
        let (major, n) = self.head()?;
        match major {
            0 | 1 | 7 => {} // integers, floats, booleans, null: the head was all of it
            2 | 3 => {
                self.take(usize::try_from(n).map_err(|_| bad("item too long"))?)?;
            }
            4 => {
                if n as usize > self.remaining() {
                    return Err(bad("array longer than its data"));
                }
                for _ in 0..n {
                    self.skip_at(depth + 1)?;
                }
            }
            5 => {
                if (n as usize).saturating_mul(2) > self.remaining() {
                    return Err(bad("map longer than its data"));
                }
                for _ in 0..n {
                    self.skip_at(depth + 1)?;
                    self.skip_at(depth + 1)?;
                }
            }
            6 => self.skip_at(depth + 1)?, // a tag wraps one item
            _ => unreachable!("major type is three bits"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(f: impl FnOnce(&mut Writer)) -> Vec<u8> {
        let mut w = Writer::new();
        f(&mut w);
        w.out
    }

    /// RFC 8949 Appendix A: the encodings the RFC itself lists.
    #[test]
    fn writes_the_rfc_examples() {
        assert_eq!(enc(|w| w.uint(0)), [0x00]);
        assert_eq!(enc(|w| w.uint(10)), [0x0a]);
        assert_eq!(enc(|w| w.uint(23)), [0x17]);
        assert_eq!(enc(|w| w.uint(24)), [0x18, 0x18]);
        assert_eq!(enc(|w| w.uint(100)), [0x18, 0x64]);
        assert_eq!(enc(|w| w.uint(1000)), [0x19, 0x03, 0xe8]);
        assert_eq!(enc(|w| w.uint(1_000_000)), [0x1a, 0x00, 0x0f, 0x42, 0x40]);
        assert_eq!(enc(|w| w.uint(1_000_000_000_000)), [0x1b, 0x00, 0x00, 0x00, 0xe8, 0xd4, 0xa5, 0x10, 0x00]);
        assert_eq!(enc(|w| w.text("")), [0x60]);
        assert_eq!(enc(|w| w.text("a")), [0x61, 0x61]);
        assert_eq!(enc(|w| w.text("IETF")), [0x64, 0x49, 0x45, 0x54, 0x46]);
        assert_eq!(enc(|w| w.bytes(&[1, 2, 3, 4])), [0x44, 1, 2, 3, 4]);
        assert_eq!(enc(|w| w.null()), [0xf6]);
        assert_eq!(enc(|w| w.array(3)), [0x83]);
    }

    #[test]
    fn reads_what_it_writes() {
        let b = enc(|w| {
            w.map(2);
            w.text("n");
            w.uint(70_000);
            w.text("k");
            w.bytes(&[9, 8]);
        });
        let mut r = Reader::new(&b);
        assert_eq!(r.map_len().unwrap(), 2);
        assert_eq!(r.text_ref().unwrap(), "n");
        assert_eq!(r.uint().unwrap(), 70_000);
        assert_eq!(r.text_ref().unwrap(), "k");
        assert_eq!(r.bytes().unwrap(), [9, 8]);
        assert!(r.is_done());
    }

    #[test]
    fn skips_values_of_every_type_it_does_not_know() {
        // {"a": -1, "b": 1.5, "c": [true, null, {"d": h'00'}], "e": 1(2)}, then 7.
        let b = [
            0xa4, 0x61, 0x61, 0x20, 0x61, 0x62, 0xf9, 0x3e, 0x00, 0x61, 0x63, 0x83, 0xf5, 0xf6, 0xa1, 0x61, 0x64, 0x41,
            0x00, 0x61, 0x65, 0xc1, 0x02, 0x07,
        ];
        let mut r = Reader::new(&b);
        r.skip().unwrap();
        assert_eq!(r.uint().unwrap(), 7);
        assert!(r.is_done());
    }

    #[test]
    fn hostile_input_is_an_error_not_a_crash_or_an_allocation() {
        assert!(Reader::new(&[0x5f]).skip().is_err(), "indefinite length");
        assert!(Reader::new(&[0x9b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).array_len().is_err(), "huge array");
        assert!(Reader::new(&[0x7a, 0xff, 0xff, 0xff, 0xff, 0x61]).text_ref().is_err(), "text longer than the data");
        assert!(Reader::new(&[0x62, 0xc3, 0x28]).text_ref().is_err(), "invalid UTF-8");
        let deep = vec![0x81u8; 64]; // 64 nested one-element arrays
        assert!(Reader::new(&deep).skip().is_err(), "nesting depth");
        assert!(Reader::new(&[]).uint().is_err(), "empty input");
    }
}
