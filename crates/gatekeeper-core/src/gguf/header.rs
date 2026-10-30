//! A minimal GGUF metadata reader (spec §15.4, §9.5).
//!
//! Responsibility: read just enough of a `.gguf` file's header to know the
//! architecture, layer count, context length and quantisation, without loading
//! any tensors. Used to pick sensible defaults for a downloaded model and to
//! estimate its VRAM footprint before it has ever run.
//! Not responsible for: tensor data or the tokenizer.
//!
//! GGUF layout: magic `GGUF`, version u32, tensor count u64, key/value count
//! u64, then key/value pairs (string key, u32 type, value), then one info
//! record per tensor (name, dimensions, type, offset). Everything is
//! little-endian. Large arrays (the vocabulary) are skipped, not loaded, and
//! tensor data is never read: only the tensor names are kept.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use crate::error::{CoreError, CoreResult};
use crate::sync::MutexExt;

/// File magic.
const MAGIC: &[u8; 4] = b"GGUF";

/// Largest string length accepted while reading, as a sanity bound against
/// corrupt files (a real key or value is far shorter).
const MAX_STRING: u64 = 64 * 1024 * 1024;

/// Most key/value pairs we will walk. Real files have a few dozen to a few
/// hundred; this stops a corrupt count from looping for hours.
const MAX_KV: u64 = 100_000;

/// Most tensor records we will walk. Large models have a few thousand; this
/// stops a corrupt count from looping for hours.
const MAX_TENSORS: u64 = 100_000;

/// Most dimensions a tensor record may declare (GGML supports four).
const MAX_DIMS: u64 = 8;

/// What we learn from the header.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GgufMeta {
    /// `general.architecture`, e.g. `qwen2` or `llama`.
    pub architecture: Option<String>,
    /// `general.name`.
    pub name: Option<String>,
    /// Human-readable quantisation from `general.file_type`, e.g. `Q4_K_M`.
    pub quantization: Option<String>,
    /// `<arch>.block_count`: number of transformer layers.
    pub block_count: Option<u32>,
    /// `<arch>.context_length`: native context size.
    pub context_length: Option<u32>,
    /// `<arch>.embedding_length`.
    pub embedding_length: Option<u32>,
    /// `<arch>.attention.head_count`.
    pub head_count: Option<u32>,
    /// `<arch>.attention.head_count_kv` (equals `head_count` without GQA).
    pub head_count_kv: Option<u32>,
    /// `<arch>.nextn_predict_layers`: multi-token-prediction layers in the model.
    pub nextn_predict_layers: Option<u32>,
    /// `tokenizer.ggml.model`, e.g. `gpt2` or `llama`.
    pub tokenizer_model: Option<String>,
    /// Number of entries in `tokenizer.ggml.tokens`, counted without loading them.
    pub vocab_size: Option<u64>,
    /// Names of every tensor in the file (the data is never read).
    pub tensor_names: Vec<String>,
}

impl GgufMeta {
    /// True if the file declares a tensor called exactly `name`.
    pub fn has_tensor(&self, name: &str) -> bool {
        self.tensor_names.iter().any(|t| t == name)
    }
}

/// A scalar value we keep.
#[derive(Debug)]
enum Value {
    /// Unsigned integer of any width.
    Uint(u64),
    /// Text.
    Text(String),
    /// An array, of which only the element count is kept.
    Count(u64),
    /// Something we do not need (floats, bools).
    Other,
}

/// Maps `general.file_type` to the quantisation name (llama.cpp's `LLAMA_FTYPE_*`).
fn file_type_name(ft: u64) -> Option<&'static str> {
    Some(match ft {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "Q2_K_S",
        22 => "IQ3_XS",
        23 => "IQ3_XXS",
        24 => "IQ1_S",
        25 => "IQ4_NL",
        26 => "IQ3_S",
        27 => "IQ3_M",
        28 => "IQ2_S",
        29 => "IQ2_M",
        30 => "IQ4_XS",
        31 => "IQ1_M",
        32 => "BF16",
        _ => return None,
    })
}

/// Reads a little-endian integer of `n` bytes.
fn read_uint<R: Read>(r: &mut R, n: usize) -> std::io::Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf[..n])?;
    Ok(u64::from_le_bytes(buf))
}

/// Reads a GGUF string (u64 length plus bytes).
fn read_string<R: Read>(r: &mut R) -> CoreResult<String> {
    let len = read_uint(r, 8).map_err(|e| CoreError::io("gguf", e))?;
    if len > MAX_STRING {
        return Err(CoreError::Other(format!("gguf string of {len} bytes is not plausible")));
    }
    let mut bytes = vec![0u8; usize::try_from(len).unwrap_or(0)];
    r.read_exact(&mut bytes).map_err(|e| CoreError::io("gguf", e))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Size in bytes of a fixed-width scalar type id, if it is one.
fn scalar_width(type_id: u32) -> Option<usize> {
    match type_id {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

/// Skips `n` bytes forward. `seek_relative` keeps the buffer of a `BufReader`,
/// so walking the ~150k strings of a vocabulary stays cheap.
fn skip<R: Seek>(r: &mut R, n: u64) -> CoreResult<()> {
    let n = i64::try_from(n).map_err(|_| CoreError::Other("gguf skip too large".into()))?;
    r.seek_relative(n).map_err(|e| CoreError::io("gguf", e))
}

/// Skips a GGUF string (u64 length plus bytes) without loading it.
fn skip_string<R: Read + Seek>(r: &mut R) -> CoreResult<()> {
    let len = read_uint(r, 8).map_err(|e| CoreError::io("gguf", e))?;
    if len > MAX_STRING {
        return Err(CoreError::Other(format!("gguf string of {len} bytes is not plausible")));
    }
    skip(r, len)
}

/// Reads one value of `type_id`, keeping integers and strings and skipping
/// everything else without loading it. Arrays yield their element count.
fn read_value<R: Read + Seek>(r: &mut R, type_id: u32) -> CoreResult<Value> {
    let io = |e| CoreError::io("gguf", e);
    match type_id {
        0 | 2 | 4 | 10 => Ok(Value::Uint(read_uint(r, scalar_width(type_id).unwrap_or(1)).map_err(io)?)),
        1 | 3 | 5 | 11 | 6 | 7 | 12 => {
            skip(r, scalar_width(type_id).unwrap_or(1) as u64)?;
            Ok(Value::Other)
        }
        8 => Ok(Value::Text(read_string(r)?)),
        9 => {
            let elem_type = u32::try_from(read_uint(r, 4).map_err(io)?).unwrap_or(u32::MAX);
            let count = read_uint(r, 8).map_err(io)?;
            if let Some(w) = scalar_width(elem_type) {
                // Fixed-width elements: one seek skips the whole array.
                let bytes =
                    count.checked_mul(w as u64).ok_or_else(|| CoreError::Other("gguf array too large".into()))?;
                skip(r, bytes)?;
            } else if elem_type == 8 {
                // Strings (the vocabulary): step over each by its length.
                for _ in 0..count {
                    skip_string(r)?;
                }
            } else {
                // Nested arrays: walk them, discarding each element.
                for _ in 0..count {
                    read_value(r, elem_type)?;
                }
            }
            Ok(Value::Count(count))
        }
        other => Err(CoreError::Other(format!("unknown gguf value type {other}"))),
    }
}

/// Reads the metadata of a GGUF file.
///
/// # Errors
/// Returns an error if the file cannot be read or is not a valid GGUF file.
pub fn read_meta(path: &Path) -> CoreResult<GgufMeta> {
    let file = File::open(path).map_err(|e| CoreError::io(path, e))?;
    read_meta_from(&mut BufReader::new(file))
}

/// Reads GGUF metadata from any seekable reader (used by tests).
///
/// # Errors
/// Returns an error for malformed input.
pub fn read_meta_from<R: Read + Seek>(r: &mut R) -> CoreResult<GgufMeta> {
    let io = |e| CoreError::io("gguf", e);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).map_err(io)?;
    if &magic != MAGIC {
        return Err(CoreError::Other("not a GGUF file (bad magic)".into()));
    }
    let version = read_uint(r, 4).map_err(io)?;
    if !(2..=3).contains(&version) {
        return Err(CoreError::Other(format!("unsupported GGUF version {version}")));
    }
    let tensor_count = read_uint(r, 8).map_err(io)?;
    let kv_count = read_uint(r, 8).map_err(io)?;
    if kv_count > MAX_KV {
        return Err(CoreError::Other(format!("implausible GGUF key count {kv_count}")));
    }
    if tensor_count > MAX_TENSORS {
        return Err(CoreError::Other(format!("implausible GGUF tensor count {tensor_count}")));
    }

    let mut meta = GgufMeta::default();
    let mut uints: Vec<(String, u64)> = Vec::new();
    for _ in 0..kv_count {
        let key = read_string(r)?;
        let type_id = u32::try_from(read_uint(r, 4).map_err(io)?).unwrap_or(u32::MAX);
        match read_value(r, type_id)? {
            Value::Text(t) => match key.as_str() {
                "general.architecture" => meta.architecture = Some(t),
                "general.name" => meta.name = Some(t),
                "tokenizer.ggml.model" => meta.tokenizer_model = Some(t),
                _ => {}
            },
            Value::Uint(n) => uints.push((key, n)),
            Value::Count(n) if key == "tokenizer.ggml.tokens" => meta.vocab_size = Some(n),
            Value::Count(_) | Value::Other => {}
        }
    }

    // Tensor records follow the key/value pairs: name, dimension count,
    // dimensions, type and data offset. Only the name is kept.
    for _ in 0..tensor_count {
        meta.tensor_names.push(read_string(r)?);
        let dims = read_uint(r, 4).map_err(io)?;
        if dims > MAX_DIMS {
            return Err(CoreError::Other(format!("implausible GGUF tensor dimension count {dims}")));
        }
        skip(r, dims * 8 + 4 + 8)?;
    }

    // Per-architecture keys are prefixed with the architecture name, which is
    // only known once every key has been read.
    let arch = meta.architecture.clone().unwrap_or_default();
    let find = |suffix: &str| {
        uints.iter().find(|(k, _)| *k == format!("{arch}.{suffix}")).and_then(|(_, v)| u32::try_from(*v).ok())
    };
    meta.block_count = find("block_count");
    meta.context_length = find("context_length");
    meta.embedding_length = find("embedding_length");
    meta.head_count = find("attention.head_count");
    meta.head_count_kv = find("attention.head_count_kv").or(meta.head_count);
    meta.nextn_predict_layers = find("nextn_predict_layers");
    meta.quantization =
        uints.iter().find(|(k, _)| k == "general.file_type").and_then(|(_, v)| file_type_name(*v)).map(str::to_owned);
    Ok(meta)
}

/// A file's metadata, remembered while the file is unchanged.
type CacheEntry = (u64, Option<SystemTime>, Arc<GgufMeta>);

/// Like [`read_meta`], but remembers the result per file until its size or
/// modification time changes. Live validation asks about the same files on
/// every edit, and walking a vocabulary is not free.
///
/// # Errors
/// Returns an error if the file cannot be read or is not a valid GGUF file.
pub fn read_meta_cached(path: &Path) -> CoreResult<Arc<GgufMeta>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    let info = std::fs::metadata(path).map_err(|e| CoreError::io(path, e))?;
    let stamp = (info.len(), info.modified().ok());
    if let Some((len, modified, meta)) = cache.lock_ok().get(path)
        && (*len, *modified) == stamp
    {
        return Ok(Arc::clone(meta));
    }
    let meta = Arc::new(read_meta(path)?);
    cache.lock_ok().insert(path.to_path_buf(), (stamp.0, stamp.1, Arc::clone(&meta)));
    Ok(meta)
}

/// Bytes of KV cache for `ctx` tokens with 16-bit cache entries.
///
/// `2 (K and V) * layers * ctx * kv_heads * head_dim * 2 bytes`, where
/// `head_dim = embedding / heads`. Returns `None` if the metadata is incomplete.
pub fn kv_cache_bytes(meta: &GgufMeta, ctx: u32) -> Option<u64> {
    let (layers, emb, heads) = (meta.block_count?, meta.embedding_length?, meta.head_count?);
    let kv_heads = meta.head_count_kv.unwrap_or(heads);
    if heads == 0 {
        return None;
    }
    let head_dim = u64::from(emb) / u64::from(heads);
    Some(2 * u64::from(layers) * u64::from(ctx) * u64::from(kv_heads) * head_dim * 2)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;

    use super::*;

    /// Builds a small but structurally real GGUF header in memory.
    pub(crate) fn sample_gguf(
        arch: &str,
        layers: u32,
        ctx: u32,
        emb: u32,
        heads: u32,
        kv_heads: u32,
        ftype: u32,
    ) -> Vec<u8> {
        sample_gguf_vocab(arch, layers, ctx, emb, heads, kv_heads, ftype, 3)
    }

    /// Like [`sample_gguf`], with a vocabulary of `vocab` tokens.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sample_gguf_vocab(
        arch: &str,
        layers: u32,
        ctx: u32,
        emb: u32,
        heads: u32,
        kv_heads: u32,
        ftype: u32,
        vocab: u64,
    ) -> Vec<u8> {
        fn s(out: &mut Vec<u8>, text: &str) {
            out.extend((text.len() as u64).to_le_bytes());
            out.extend(text.as_bytes());
        }
        fn kv_u32(out: &mut Vec<u8>, key: &str, v: u32) {
            s(out, key);
            out.extend(4u32.to_le_bytes());
            out.extend(v.to_le_bytes());
        }
        let mut b = Vec::new();
        b.extend(b"GGUF");
        b.extend(3u32.to_le_bytes());
        b.extend(0u64.to_le_bytes()); // tensors
        b.extend(10u64.to_le_bytes()); // kv count
        s(&mut b, "general.architecture");
        b.extend(8u32.to_le_bytes());
        s(&mut b, arch);
        s(&mut b, "general.name");
        b.extend(8u32.to_le_bytes());
        s(&mut b, "Test Model");
        kv_u32(&mut b, "general.file_type", ftype);
        // A string array like the tokenizer vocabulary, to prove it is skipped.
        s(&mut b, "tokenizer.ggml.tokens");
        b.extend(9u32.to_le_bytes());
        b.extend(8u32.to_le_bytes());
        b.extend(vocab.to_le_bytes());
        for i in 0..vocab {
            s(&mut b, &format!("t{i}"));
        }
        // A float array and a float scalar, also skipped.
        s(&mut b, "tokenizer.ggml.scores");
        b.extend(9u32.to_le_bytes());
        b.extend(6u32.to_le_bytes());
        b.extend(2u64.to_le_bytes());
        b.extend([0u8; 8]);
        s(&mut b, "general.some_float");
        b.extend(6u32.to_le_bytes());
        b.extend(1.5f32.to_le_bytes());
        kv_u32(&mut b, &format!("{arch}.block_count"), layers);
        kv_u32(&mut b, &format!("{arch}.context_length"), ctx);
        kv_u32(&mut b, &format!("{arch}.embedding_length"), emb);
        kv_u32(&mut b, &format!("{arch}.attention.head_count"), heads);
        // head_count_kv is the 11th pair; fix the count accordingly.
        kv_u32(&mut b, &format!("{arch}.attention.head_count_kv"), kv_heads);
        b[16..24].copy_from_slice(&11u64.to_le_bytes());
        b
    }

    /// Appends key/value pairs and tensor records to a [`sample_gguf`] buffer
    /// and patches the counts in the header.
    pub(crate) fn extend_gguf(
        mut b: Vec<u8>,
        uints: &[(&str, u32)],
        texts: &[(&str, &str)],
        tensors: &[&str],
    ) -> Vec<u8> {
        fn s(out: &mut Vec<u8>, text: &str) {
            out.extend((text.len() as u64).to_le_bytes());
            out.extend(text.as_bytes());
        }
        let kv = u64::from_le_bytes(b[16..24].try_into().expect("kv count")) + (uints.len() + texts.len()) as u64;
        b[16..24].copy_from_slice(&kv.to_le_bytes());
        b[8..16].copy_from_slice(&(tensors.len() as u64).to_le_bytes());
        for (key, v) in uints {
            s(&mut b, key);
            b.extend(4u32.to_le_bytes());
            b.extend(v.to_le_bytes());
        }
        for (key, v) in texts {
            s(&mut b, key);
            b.extend(8u32.to_le_bytes());
            s(&mut b, v);
        }
        for name in tensors {
            s(&mut b, name);
            b.extend(2u32.to_le_bytes()); // two dimensions
            b.extend([0u8; 16]);
            b.extend(0u32.to_le_bytes()); // type
            b.extend(0u64.to_le_bytes()); // offset
        }
        b
    }

    #[test]
    fn reads_vocab_size_tokenizer_model_and_nextn_layers() {
        let bytes = extend_gguf(
            sample_gguf("qwen3", 36, 8192, 4096, 32, 8, 15),
            &[("qwen3.nextn_predict_layers", 1)],
            &[("tokenizer.ggml.model", "gpt2")],
            &[],
        );
        let meta = read_meta_from(&mut Cursor::new(bytes)).expect("parse");
        assert_eq!(meta.vocab_size, Some(3), "the sample vocabulary has three tokens");
        assert_eq!(meta.tokenizer_model.as_deref(), Some("gpt2"));
        assert_eq!(meta.nextn_predict_layers, Some(1));
        let plain = read_meta_from(&mut Cursor::new(sample_gguf("a", 1, 1, 1, 1, 1, 0))).expect("parse");
        assert_eq!(plain.nextn_predict_layers, None);
    }

    #[test]
    fn reads_tensor_names_without_reading_tensor_data() {
        let names = ["token_embd.weight", "blk.35.nextn.eh_proj.weight"];
        let bytes = extend_gguf(sample_gguf("llama", 36, 1, 1, 1, 1, 0), &[], &[], &names);
        let meta = read_meta_from(&mut Cursor::new(bytes)).expect("parse");
        assert_eq!(meta.tensor_names, names);
        assert!(meta.has_tensor("blk.35.nextn.eh_proj.weight"));
        assert!(!meta.has_tensor("blk.34.nextn.eh_proj.weight"));
    }

    #[test]
    fn implausible_tensor_count_is_rejected() {
        let mut bytes = sample_gguf("llama", 1, 1, 1, 1, 1, 0);
        bytes[8..16].copy_from_slice(&(MAX_TENSORS + 1).to_le_bytes());
        assert!(read_meta_from(&mut Cursor::new(bytes)).is_err());
        // A count the file cannot back up fails on the missing records, not by stalling.
        let mut short = sample_gguf("llama", 1, 1, 1, 1, 1, 0);
        short[8..16].copy_from_slice(&50_000u64.to_le_bytes());
        assert!(read_meta_from(&mut Cursor::new(short)).is_err());
    }

    #[test]
    fn cached_read_reflects_a_changed_file() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("m.gguf");
        std::fs::write(&path, sample_gguf("llama", 4, 1, 1, 1, 1, 0)).expect("write");
        assert_eq!(read_meta_cached(&path).expect("first").block_count, Some(4));
        let changed = extend_gguf(sample_gguf("llama", 9, 1, 1, 1, 1, 0), &[], &[], &["a.weight"]);
        std::fs::write(&path, changed).expect("rewrite");
        assert_eq!(read_meta_cached(&path).expect("second").block_count, Some(9), "a new size busts the cache");
    }

    #[test]
    fn reads_the_fields_we_need_and_skips_large_arrays() {
        let bytes = sample_gguf("qwen2", 24, 32768, 896, 14, 2, 15);
        let meta = read_meta_from(&mut Cursor::new(bytes)).expect("parse");
        assert_eq!(meta.architecture.as_deref(), Some("qwen2"));
        assert_eq!(meta.name.as_deref(), Some("Test Model"));
        assert_eq!(meta.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!((meta.block_count, meta.context_length, meta.embedding_length), (Some(24), Some(32768), Some(896)));
        assert_eq!((meta.head_count, meta.head_count_kv), (Some(14), Some(2)));
    }

    #[test]
    fn kv_cache_estimate_uses_grouped_query_heads() {
        let meta = read_meta_from(&mut Cursor::new(sample_gguf("llama", 32, 8192, 4096, 32, 8, 15))).expect("parse");
        // 2 * 32 layers * 8192 ctx * 8 kv-heads * 128 head-dim * 2 bytes = 1 GiB
        assert_eq!(kv_cache_bytes(&meta, 8192), Some(1024 * 1024 * 1024));
        assert_eq!(kv_cache_bytes(&GgufMeta::default(), 8192), None);
    }

    #[test]
    fn missing_kv_heads_default_to_full_heads() {
        let mut meta = read_meta_from(&mut Cursor::new(sample_gguf("llama", 2, 100, 64, 4, 4, 1))).expect("parse");
        meta.head_count_kv = None;
        assert_eq!(kv_cache_bytes(&meta, 10), Some(2 * 2 * 10 * 4 * 16 * 2));
    }

    #[test]
    fn rejects_bad_magic_and_versions_and_truncation() {
        assert!(read_meta_from(&mut Cursor::new(b"NOPE\0\0\0\0".to_vec())).is_err());
        let mut v1 = sample_gguf("a", 1, 1, 1, 1, 1, 0);
        v1[4..8].copy_from_slice(&1u32.to_le_bytes());
        assert!(read_meta_from(&mut Cursor::new(v1)).is_err());
        let full = sample_gguf("a", 1, 1, 1, 1, 1, 0);
        assert!(read_meta_from(&mut Cursor::new(full[..full.len() / 2].to_vec())).is_err());
    }

    #[test]
    fn unknown_file_types_have_no_name() {
        let meta = read_meta_from(&mut Cursor::new(sample_gguf("a", 1, 1, 1, 1, 1, 9999))).expect("parse");
        assert_eq!(meta.quantization, None);
    }
}
