//! where the bitcoin goes, checked before it is sent: a typo in an address or an invoice loses
//! the coins for good. each method has its own formats, and methods we don't know are taken as
//! they are, to be agreed in the chat.

use sha2::{Digest, Sha256};
use sha3::Keccak256;

/// the destination as it should be sent, or why it can't be. `sats` is what the trade is for,
/// to check it against an invoice that says an amount.
pub fn parse(method: &str, input: &str, sats: Option<u64>) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("the details are empty".into());
    }
    match method {
        // "onchain" is how it was called before
        "on-chain" | "onchain" => {
            let s = strip_uri(s, "bitcoin:");
            onchain(s).ok_or_else(|| "that is not a valid bitcoin address".into())
        }
        "lightning" => lightning(strip_uri(s, "lightning:"), sats),
        "liquid" => {
            let s = strip_uri(s, "liquidnetwork:");
            liquid(s).ok_or_else(|| "that is not a valid liquid address".into())
        }
        "arkade" => {
            let s = strip_uri(s, "ark:");
            arkade(s).ok_or_else(|| "that is not a valid arkade address (ark1…)".into())
        }
        "rootstock" => rootstock(s),
        _ => Ok(s.to_string()),
    }
}

/// "bitcoin:bc1…?amount=…" -> "bc1…"
fn strip_uri<'a>(s: &'a str, scheme: &str) -> &'a str {
    match s.get(..scheme.len()) {
        Some(p) if p.eq_ignore_ascii_case(scheme) => {
            let rest = &s[scheme.len()..];
            rest.split('?').next().unwrap_or(rest)
        }
        _ => s,
    }
}

fn onchain(s: &str) -> Option<String> {
    if let Some(addr) = bech32_lower(s) {
        segwit(&addr, "bc")?;
        return (addr.len() <= 90).then_some(addr);
    }
    let payload = base58check(s)?;
    (payload.len() == 21 && matches!(payload[0], 0 | 5)).then(|| s.to_string())
}

fn lightning(s: &str, sats: Option<u64>) -> Result<String, String> {
    if s.contains('@') {
        return lightning_address(s)
            .ok_or_else(|| "that is not a valid lightning address (name@domain)".into());
    }
    let invalid = || "that is not a valid lightning invoice or address".to_string();
    let lower = bech32_lower(s).ok_or_else(invalid)?;
    let (hrp, data, residue) = decode(&lower, BECH32).ok_or_else(invalid)?;
    if residue != 1 {
        return Err(invalid());
    }
    if hrp == "lnurl" {
        return Ok(lower);
    }
    let Some(amount) = hrp.strip_prefix("lnbc") else {
        return Err("that is not a mainnet lightning invoice (lnbc…)".into());
    };
    // a timestamp and a signature at least
    if data.len() < 7 + 104 {
        return Err(invalid());
    }
    let msat = invoice_msat(amount).ok_or_else(invalid)?;
    if let (Some(msat), Some(sats)) = (msat, sats) {
        if sats.checked_mul(1000) != Some(msat) {
            return Err(format!(
                "the invoice is for {} sats, the trade is for {} sats",
                crate::prices::thousands(msat as f64 / 1000.0),
                crate::prices::thousands(sats as f64)
            ));
        }
    }
    Ok(lower)
}

/// the amount in the human readable part of an invoice, after "lnbc". none when it is empty,
/// for an invoice that lets the payer choose.
fn invoice_msat(amount: &str) -> Option<Option<u64>> {
    if amount.is_empty() {
        return Some(None);
    }
    let (digits, unit) = match amount.as_bytes()[amount.len() - 1] {
        c @ (b'm' | b'u' | b'n' | b'p') => (&amount[..amount.len() - 1], Some(c)),
        _ => (amount, None),
    };
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) || digits.starts_with('0') {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    // in thousandths of a msat, as a pico-bitcoin is a tenth of one
    let per = match unit {
        None => 100_000_000_000_000,
        Some(b'm') => 100_000_000_000,
        Some(b'u') => 100_000_000,
        Some(b'n') => 100_000,
        _ => 100,
    };
    let total = n.checked_mul(per)?;
    (total % 1000 == 0).then_some(Some(total / 1000))
}

fn lightning_address(s: &str) -> Option<String> {
    let s = s.to_ascii_lowercase();
    let (name, domain) = s.split_once('@')?;
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.+".contains(&c));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
        && labels
            .last()
            .is_some_and(|tld| tld.len() >= 2 && tld.bytes().all(|c| c.is_ascii_alphabetic()));
    (name_ok && domain_ok).then_some(s)
}

fn liquid(s: &str) -> Option<String> {
    if let Some(addr) = bech32_lower(s) {
        if addr.starts_with("lq1") {
            let (hrp, data, residue) = decode(&addr, BLECH32)?;
            let (version, program) = witness(&hrp, &data, "lq")?;
            // the blinding key comes before the witness program
            let (key, program) = program.split_at_checked(33)?;
            let constant = if version == 0 { 1 } else { BLECH32M_CONST };
            return (residue == constant
                && matches!(key[0], 2 | 3)
                && program_ok(version, program.len()))
            .then_some(addr);
        }
        segwit(&addr, "ex")?;
        return Some(addr);
    }
    let payload = base58check(s)?;
    let ok = match payload.as_slice() {
        [57 | 39, ..] => payload.len() == 21,
        // confidential: the prefix, the version, the blinding key and the hash
        [12, 57 | 39, 2 | 3, ..] => payload.len() == 55,
        _ => false,
    };
    ok.then(|| s.to_string())
}

fn arkade(s: &str) -> Option<String> {
    let addr = bech32_lower(s)?;
    let (hrp, data, residue) = decode(&addr, BECH32)?;
    // a version, the server's key and the vtxo's
    (hrp == "ark" && residue == BECH32M_CONST && from_5bit(&data)?.len() == 65).then_some(addr)
}

/// an evm address. mixed case is a checksum (eip-1191 for rootstock, or eip-55 as plenty of
/// wallets show) and has to match, a single case can't be checked.
fn rootstock(s: &str) -> Result<String, String> {
    let invalid = || "that is not a valid rootstock address (0x…)".to_string();
    let hex = s.strip_prefix("0x").ok_or_else(invalid)?;
    if hex.len() != 40 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let has_lower = hex.bytes().any(|c| c.is_ascii_lowercase());
    let has_upper = hex.bytes().any(|c| c.is_ascii_uppercase());
    if has_lower && has_upper {
        let lower = hex.to_ascii_lowercase();
        let matches = |prefix: &str| checksummed(&lower, prefix) == hex;
        if !matches("300x") && !matches("") {
            return Err("the address checksum is wrong, check every character".into());
        }
    }
    Ok(format!("0x{hex}"))
}

fn checksummed(lower: &str, prefix: &str) -> String {
    let hash = Keccak256::digest(format!("{prefix}{lower}").as_bytes());
    lower
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let nibble = (hash[i / 2] >> if i % 2 == 0 { 4 } else { 0 }) & 0xf;
            if nibble >= 8 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------- bech32 & co

const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const BECH32M_CONST: u64 = 0x2bc8_30a3;
const BLECH32M_CONST: u64 = 0x455_972a_3350_f7a1;

/// the generators, the checksum length, and how many bits the checksum takes
struct Code {
    generators: [u64; 5],
    checksum_len: usize,
    bits: u32,
}

const BECH32: Code = Code {
    generators: [
        0x3b6a_57b2,
        0x2650_8e6d,
        0x1ea1_19fa,
        0x3d42_33dd,
        0x2a14_62b3,
    ],
    checksum_len: 6,
    bits: 30,
};

const BLECH32: Code = Code {
    generators: [
        0x7d_52fb_a40b_d886,
        0x5e_8dbf_1a03_950c,
        0x1c_3a3c_7407_2a18,
        0x38_5d72_fa0e_5139,
        0x70_93e5_a608_865b,
    ],
    checksum_len: 12,
    bits: 60,
};

/// the string in lowercase if it looks like bech32: one case only (an all-caps one comes from a
/// qr code) and a separator
fn bech32_lower(s: &str) -> Option<String> {
    let has_lower = s.bytes().any(|c| c.is_ascii_lowercase());
    let has_upper = s.bytes().any(|c| c.is_ascii_uppercase());
    (s.contains('1') && !(has_lower && has_upper)).then(|| s.to_ascii_lowercase())
}

/// the human readable part, the data without the checksum, and the residue of the checksum,
/// which tells the variant apart
fn decode(s: &str, code: Code) -> Option<(String, Vec<u8>, u64)> {
    let (hrp, data) = s.rsplit_once('1')?;
    if hrp.is_empty()
        || !hrp.bytes().all(|c| (33..=126).contains(&c))
        || data.len() < code.checksum_len
    {
        return None;
    }
    let data: Vec<u8> = data
        .bytes()
        .map(|c| CHARSET.iter().position(|x| *x == c).map(|i| i as u8))
        .collect::<Option<_>>()?;
    let mut values: Vec<u8> = hrp.bytes().map(|c| c >> 5).collect();
    values.push(0);
    values.extend(hrp.bytes().map(|c| c & 31));
    values.extend(&data);
    let residue = polymod(&values, &code);
    let len = data.len() - code.checksum_len;
    Some((hrp.to_string(), data[..len].to_vec(), residue))
}

fn polymod(values: &[u8], code: &Code) -> u64 {
    let shift = code.bits - 5;
    let mask = (1u64 << shift) - 1;
    let mut chk: u64 = 1;
    for v in values {
        let top = chk >> shift;
        chk = ((chk & mask) << 5) ^ *v as u64;
        for (i, g) in code.generators.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= g;
            }
        }
    }
    chk
}

/// a segwit address of `hrp`, with the checksum variant its version calls for
fn segwit(addr: &str, hrp: &str) -> Option<()> {
    let (got_hrp, data, residue) = decode(addr, BECH32)?;
    let (version, program) = witness(&got_hrp, &data, hrp)?;
    let constant = if version == 0 { 1 } else { BECH32M_CONST };
    (residue == constant && program_ok(version, program.len())).then_some(())
}

/// the witness version and program in the data of an address of `hrp`
fn witness(got_hrp: &str, data: &[u8], hrp: &str) -> Option<(u8, Vec<u8>)> {
    if got_hrp != hrp {
        return None;
    }
    let (version, program) = data.split_first()?;
    (*version <= 16).then_some(())?;
    Some((*version, from_5bit(program)?))
}

fn program_ok(version: u8, len: usize) -> bool {
    match version {
        0 => len == 20 || len == 32,
        _ => (2..=40).contains(&len),
    }
}

/// 5-bit groups to bytes, without padding beyond what fills the last one
fn from_5bit(data: &[u8]) -> Option<Vec<u8>> {
    let (mut acc, mut bits, mut out) = (0u32, 0u32, vec![]);
    for v in data {
        acc = (acc << 5) | *v as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    (bits < 5 && acc & ((1 << bits) - 1) == 0).then_some(out)
}

// ---------------------------------------------------------------------------- base58

/// the payload of a base58check string, its checksum verified
fn base58check(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut bytes: Vec<u8> = vec![];
    for c in s.bytes() {
        let mut carry = ALPHABET.iter().position(|x| *x == c)? as u32;
        for b in bytes.iter_mut().rev() {
            carry += *b as u32 * 58;
            *b = carry as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.insert(0, carry as u8);
            carry >>= 8;
        }
    }
    let zeros = s.bytes().take_while(|c| *c == b'1').count();
    let mut full = vec![0u8; zeros];
    full.extend(bytes);
    if full.len() < 5 {
        return None;
    }
    let (payload, checksum) = full.split_at(full.len() - 4);
    let hash = Sha256::digest(Sha256::digest(payload));
    (hash[..4] == *checksum).then(|| payload.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(method: &str, s: &str) -> bool {
        parse(method, s, None).is_ok()
    }

    #[test]
    fn onchain() {
        for a in [
            "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2",
            "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy",
            "BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4",
            "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3",
            "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0",
            "bitcoin:bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4?amount=0.001",
        ] {
            assert!(ok("on-chain", a), "{a}");
        }
        assert_eq!(
            parse(
                "on-chain",
                "BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4",
                None
            )
            .unwrap(),
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
        );
        for a in [
            "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN3",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5",
            "bc1qw508d6qejxtdg4Y5r3zarvary0c5xw7kv8f3t4",
            // v1 with the bech32 checksum instead of bech32m
            "bc1pw508d6qejxtdg4y5r3zarvary0c5xw7kw508d6qejxtdg4y5r3zarvary0c5xw7k7grplx",
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
            "hello",
        ] {
            assert!(!ok("on-chain", a), "{a}");
        }
    }

    #[test]
    fn lightning() {
        let any = "lnbc1pvjluezsp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygspp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdpl2pkx2ctnv5sxxmmwwd5kgetjypeh2ursdae8g6twvus8g6rfwvs8qun0dfjkxaq9qrsgq357wnc5r2ueh7ck6q93dj32dlqnls087fxdwk8qakdyafkq3yap9us6v52vjjsrvywa6rt52cm9r9zqt8r2t7mlcwspyetp5h2tztugp9lfyql";
        let quarter = "lnbc2500u1pvjluezsp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygspp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdq5xysxxatsyp3k7enxv4jsxqzpu9qrsgquk0rl77nj30yxdy8j9vdx85fkpmdla2087ne0xh8nhedh8w27kyke0lp53ut353s06fv3qfegext0eh0ymjpf39tuven09sam30g4vgpfna3rh";
        assert!(parse("lightning", any, Some(1234)).is_ok());
        assert!(parse("lightning", &format!("lightning:{quarter}"), Some(250_000)).is_ok());
        assert!(parse("lightning", &quarter.to_uppercase(), None).is_ok());
        assert_eq!(
            parse("lightning", quarter, Some(1000)).unwrap_err(),
            "the invoice is for 250,000 sats, the trade is for 1,000 sats"
        );
        for bad in [
            // bad checksum, mixed case, too short, a bad multiplier, sub-msat precision, testnet
            "lnbc2500u1pvjluezpp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdpquwpc4curk03c9wlrswe78q4eyqc7d8d0xqzpuyk0sg5g70me25alkluzd2x62aysf2pyy8edtjeevuv4p2d5p76r4zkmneet7uvyakky2zr4cusd45tftc9c5fh0nnqpnl2jfll544esqchsrnt",
            "LNBC2500u1pvjluezpp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdpquwpc4curk03c9wlrswe78q4eyqc7d8d0xqzpuyk0sg5g70me25alkluzd2x62aysf2pyy8edtjeevuv4p2d5p76r4zkmneet7uvyakky2zr4cusd45tftc9c5fh0nnqpnl2jfll544esqchsrny",
            "lnbc1pvjluezpp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdpl2pkx2ctnv5sxxmmwwd5kgetjypeh2ursdae8g6na6hlh",
            "lnbc2500x1pvjluezpp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdq5xysxxatsyp3k7enxv4jsxqzpusp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs9qrsgqrrzc4cvfue4zp3hggxp47ag7xnrlr8vgcmkjxk3j5jqethnumgkpqp23z9jclu3v0a7e0aruz366e9wqdykw6dxhdzcjjhldxq0w6wgqcnu43j",
            "lnbc2500000001p1pvjluezpp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdq5xysxxatsyp3k7enxv4jsxqzpusp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs9qrsgq0lzc236j96a95uv0m3umg28gclm5lqxtqqwk32uuk4k6673k6n5kfvx3d2h8s295fad45fdhmusm8sjudfhlf6dcsxmfvkeywmjdkxcp99202x",
            "lntb20m1pvjluezsp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygshp58yjmdan79s6qqdhdzgynm4zwqd5d7xmw5fk98klysy043l2ahrqspp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqfpp3x9et2e20v6pu37c5d9vax37wxq72un989qrsgqdj545axuxtnfemtpwkc45hx9d2ft7x04mt8q7y6t0k2dge9e7h8kpy9p34ytyslj3yu569aalz2xdk8xkd7ltxqld94u8h2esmsmacgpghe9k8",
        ] {
            assert!(!ok("lightning", bad), "{bad}");
        }
        assert_eq!(
            parse("lightning", " Satoshi@Example.com ", None).unwrap(),
            "satoshi@example.com"
        );
        for bad in [
            "@example.com",
            "me@localhost",
            "me@exa_mple.com",
            "a b@x.com",
        ] {
            assert!(!ok("lightning", bad), "{bad}");
        }
    }

    #[test]
    fn liquid() {
        for a in [
            "GqiQRsPEyJLAsEBFB5R34KHuqxDNkG3zur",
            "QFq3vvrr6Ub2KAyb3LdoCxEQvKukB6nN9i",
            "H55PJDhj6JpR5k9wViXGEX4nga8WmhXtnD",
            "VJLDwMVWXg8RKq4mRe3YFNTAEykVN6V8x5MRUKKoC3nfRnbpnZeiG3jygMC6A4Gw967GY5EotJ4Rau2F",
            "VTptY6cqJbusNpL5xvo8VL38nLX9PGDjfYQfqhu9EaA7FtuidkWyQzMHY9jzZrpBcCXT437vM6V4N8kh",
            "ex1q7gkeyjut0mrxc3j0kjlt7rmcnvsh0gt45d3fud",
            "ex1p8qs0qcn25l2y6yvtc5t95rr8w9pndcj64c8rkutnvkcvdp6gh02qa4lw5j",
            "lq1qqf8er278e6nyvuwtgf39e6ewvdcnjupn9a86rzpx655y5lhkt0walu3djf9cklkxd3ryld97hu8h3xepw7sh2rlu7q45dcew5",
            "lq1qqgft7r4ytdenml0gaj67393sd3qkt3nxex0ut5dt3plhzwf6jaww4casc3pf3lquzjd0haxgn9hmjfp84eq7geymjdx2f9verdu99wz47jmkmgmr9a4s",
            "lq1pqgft7r4ytdenml0gaj67393sd3qkt3nxex0ut5dt3plhzwf6jaww5wpq7p3x4f75f5gch3gktgxxwu2rxm394tsw8dchxedsc6r53w75375l4kfvf08y",
        ] {
            assert!(ok("liquid", a), "{a}");
        }
        for bad in [
            "lq1qqf8er278e6nyvuwtgf39e6ewvdcnjupn9a86rzpx655y5lhkt0walu3djf9cklkxd3ryld97hu8h3xepw7sh2rlu7q45dcew6",
            "GqiQRsPEyJLAsEBFB5R34KHuqxDNkG3zus",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        ] {
            assert!(!ok("liquid", bad), "{bad}");
        }
    }

    #[test]
    fn arkade() {
        // a version, then two 32 byte keys
        let data: Vec<u8> = (0..65).map(|i| i as u8).collect();
        let addr = encode_bech32m("ark", &data);
        assert!(ok("arkade", &addr));
        assert!(!ok("arkade", &encode_bech32m("tark", &data)));
        assert!(!ok("arkade", &encode_bech32m("ark", &data[..64])));
        let mut typo = addr.clone().into_bytes();
        let last = typo.len() - 1;
        typo[last] = if typo[last] == b'q' { b'p' } else { b'q' };
        assert!(!ok("arkade", &String::from_utf8(typo).unwrap()));
    }

    fn encode_bech32m(hrp: &str, bytes: &[u8]) -> String {
        let (mut acc, mut bits, mut data) = (0u32, 0u32, vec![]);
        for b in bytes {
            acc = (acc << 8) | *b as u32;
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                data.push(((acc >> bits) & 31) as u8);
            }
        }
        if bits > 0 {
            data.push(((acc << (5 - bits)) & 31) as u8);
        }
        let mut values: Vec<u8> = hrp.bytes().map(|c| c >> 5).collect();
        values.push(0);
        values.extend(hrp.bytes().map(|c| c & 31));
        values.extend(&data);
        values.extend([0; 6]);
        let pm = polymod(&values, &BECH32) ^ BECH32M_CONST;
        data.extend((0..6).map(|i| ((pm >> (5 * (5 - i))) & 31) as u8));
        let chars: String = data.iter().map(|d| CHARSET[*d as usize] as char).collect();
        format!("{hrp}1{chars}")
    }

    #[test]
    fn rootstock() {
        // eip-1191 for rsk mainnet, eip-55, and a single case
        for a in [
            "0x5aaEB6053f3e94c9b9a09f33669435E7ef1bEAeD",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed",
        ] {
            assert!(ok("rootstock", a), "{a}");
        }
        for bad in [
            "0x5aaEB6053f3e94c9b9a09f33669435E7ef1bEAED",
            "0x5aaeb6053f3e94c9b9a09f33669435e7ef1bea",
            "5aaeb6053f3e94c9b9a09f33669435e7ef1beaed",
        ] {
            assert!(!ok("rootstock", bad), "{bad}");
        }
    }

    #[test]
    fn other_methods_pass_through() {
        assert_eq!(parse("cassis", " anything ", None).unwrap(), "anything");
        assert_eq!(parse("", "ask me", None).unwrap(), "ask me");
        assert!(parse("cassis", "  ", None).is_err());
    }
}
