use std::{
    ffi::CString,
    fmt::Debug,
    io::{BufRead, BufReader},
    mem::MaybeUninit,
    ptr::NonNull,
};

use anyhow::{bail, Context, Result};

use clap::value_parser;
use zentool_sys::{self as zt, free_patch_file, load_patch_file, save_patch_file};

#[repr(C)]
struct GlobalOpts {
    verbose: std::ffi::c_int,
    quiet: std::ffi::c_int,
    debug: std::ffi::c_int,
    infile: *mut std::ffi::c_char,
    outfile: *mut std::ffi::c_char,
}

unsafe impl Sync for GlobalOpts {}

#[no_mangle]
#[allow(non_upper_case_globals)]
static options: GlobalOpts = GlobalOpts {
    verbose: 0,
    quiet: 0,
    debug: 0,
    infile: std::ptr::null_mut(),
    outfile: std::ptr::null_mut(),
};

fn peek_word(text: &str) -> &str {
    let end = text
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(text.len());
    &text[..end]
}

fn take_word<'a>(text: &mut &'a str) -> &'a str {
    let word = peek_word(text);
    *text = &text[word.len()..];
    word
}

fn take_number<'a>(text: &mut &'a str) -> Result<u16> {
    let word = take_word(text);
    if let Some(hex) = word.strip_prefix("0x") {
        u16::from_str_radix(hex, 16).map_err(Into::into)
    } else {
        word.parse::<u16>().map_err(Into::into)
    }
}

#[repr(transparent)]
struct Seqword(zt::seqword);

impl Seqword {
    fn new(target: u32, action: u32, nodelay: bool) -> Self {
        let mut result = Self::from_raw(0);
        result.set_target(target);
        result.set_action(action);
        result.set_nodelay(nodelay);
        result
    }

    const fn from_raw(value: u32) -> Self {
        Self(zt::seqword { value })
    }

    fn target(&self) -> u32 {
        unsafe { self.0.__bindgen_anon_1.target() }
    }

    fn set_target(&mut self, value: u32) {
        assert!(value <= 0x2000);
        unsafe {
            self.0.__bindgen_anon_1.set_target(value);
        }
    }

    fn action(&self) -> u32 {
        unsafe { self.0.flags.action() }
    }

    fn set_action(&mut self, value: u32) {
        assert!(value <= 0xF);
        unsafe { self.0.flags.set_action(value as u32) }
    }

    fn nodelay(&self) -> bool {
        unsafe { self.0.flags.nodelay() != 0 }
    }

    fn set_nodelay(&mut self, value: bool) {
        unsafe { self.0.flags.set_nodelay(value as u32) }
    }

    const SEQ_RELATIVE: u32 = zt::SEQ_RELATIVE;
    const SEQ_ABSOLUTE: u32 = zt::SEQ_ABSOLUTE;

    // TODO: What does this actually do?
    const NOP_QUAD_DEFAULT: Self = Self::from_raw(0x04000001);
    // Is it 7 or 0x100002... let's do both actually
    const RFE: Self = Self::from_raw(0x100007);
}

impl Debug for Seqword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seqword")
            .field("target", &self.target())
            .field("action", &self.action())
            .field("nodelay", &self.nodelay())
            .finish_non_exhaustive()
    }
}

#[repr(transparent)]
struct UCodeWord(zt::_bindgen_ty_2);

impl UCodeWord {
    // TODO: I'm curious what these actually (this and the nop seqword) decode to
    const NOP: Self = Self::from_raw(0x007f9c0000000000);

    const fn from_raw(value: u64) -> Self {
        Self(zt::_bindgen_ty_2 {
            value: zt::_bindgen_ty_2__bindgen_ty_1 { q: value },
        })
    }
}

impl Debug for UCodeWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:032X}", unsafe { self.0.value.q })
    }
}

#[repr(C)]
#[derive(Debug)]
struct UCodeQuad {
    pub words: [UCodeWord; 4],
    pub seq: Seqword,
}

impl UCodeQuad {
    const NOP: Self = Self {
        words: [UCodeWord::NOP; 4],
        seq: Seqword::NOP_QUAD_DEFAULT,
    };
}

#[derive(Debug)]
struct AsmQuad {
    name: Option<Box<str>>,
    ops: UCodeQuad,
}

enum State {
    Initial,
    AfterMatch(u16),
    InQuad(AsmQuad, u8),
    AfterSeq,
}

#[derive(Debug)]
struct AsmMatch {
    value: u16,
    target: usize,
}

struct Assembler {
    quads: Vec<AsmQuad>,
    matches: Vec<AsmMatch>,
    current: State,
}

impl Assembler {
    fn new() -> Self {
        Self {
            quads: Vec::new(),
            matches: Vec::new(),
            current: State::Initial,
        }
    }

    fn process_line(&mut self, mut line: &str) -> Result<()> {
        line = line.trim_start();
        while let Some(pos) = line.rfind(';') {
            line = &line[..pos];
        }
        line = line.trim_end();

        if line.is_empty() {
            return Ok(());
        }

        if line.starts_with('.') {
            line = &line[1..];
            let command = take_word(&mut line);
            match command {
                "quad" => {
                    line = line.trim_start();
                    let name = take_word(&mut line);

                    let new = State::InQuad(
                        AsmQuad {
                            name: (!name.is_empty()).then(|| name.into()),
                            ops: UCodeQuad {
                                words: [UCodeWord::NOP; 4],
                                seq: Seqword::new(1, Seqword::SEQ_RELATIVE, true),
                            },
                        },
                        0,
                    );

                    match std::mem::replace(&mut self.current, new) {
                        State::InQuad(quad, _) => self.quads.push(quad),
                        State::AfterMatch(value) => {
                            self.matches.push(AsmMatch {
                                value,
                                target: self.quads.len(),
                            });
                        }
                        State::Initial | State::AfterSeq => {}
                    }

                    Ok(())
                }
                "match" => {
                    line = line.trim_start();
                    let value = take_number(&mut line)?;

                    match std::mem::replace(&mut self.current, State::AfterMatch(value)) {
                        State::InQuad(quad, _) => self.quads.push(quad),
                        State::AfterMatch(..) => {
                            bail!("duplicate .match on quad")
                        }
                        State::Initial | State::AfterSeq => {}
                    }

                    Ok(())
                }
                "seq" => {
                    let mut quad = match std::mem::replace(&mut self.current, State::AfterSeq) {
                        State::InQuad(q, _) => q,
                        State::Initial | State::AfterMatch(..) | State::AfterSeq => {
                            bail!("sequence word outside of quad")
                        }
                    };

                    line = line.trim_start();
                    let kind = take_word(&mut line);
                    match kind {
                        "rfe" => {
                            quad.ops.seq = Seqword::RFE;
                        }
                        _ => bail!("unknown sequence word kind"),
                    }

                    self.quads.push(quad);
                    self.current = State::AfterSeq;

                    Ok(())
                }
                _ => bail!("unknown dot command .{command}"),
            }
        } else {
            let (quad, init_ops) = match self.current {
                State::Initial | State::AfterMatch(..) => {
                    bail!("instruction outside of quad")
                }
                State::InQuad(_, init_ops) if init_ops >= 4 => {
                    bail!("quad cannot contain more than four ops")
                }
                State::InQuad(ref mut quad, ref mut ops) => (quad, ops),
                State::AfterSeq => {
                    bail!("cannot add instructions after sequence word")
                }
            };

            let ins = peek_word(line);
            let cstr = CString::new(line)?;
            let op = unsafe {
                let mut out = MaybeUninit::zeroed();
                if !zt::zen_assemble_line(cstr.as_ptr(), out.as_mut_ptr()) {
                    bail!("invalid instruction line: {line}")
                }
                UCodeWord(std::mem::transmute::<zt::BaseOp, zt::_bindgen_ty_2>(
                    out.assume_init(),
                ))
            };

            quad.ops.words[usize::from(*init_ops)] = op;
            *init_ops += 1;
            Ok(())
        }
    }

    fn process_eof(&mut self) -> Result<()> {
        Ok(())
    }

    unsafe fn apply(&self, mut patch: NonNull<zt::patch>) -> Result<()> {
        let m = patch.as_mut();

        // println!("{:#?}", self.quads);
        // println!("{:#?}", self.matches);

        for mr in &self.matches {
            if mr.target >= 2 * m.nmatch as usize {
                bail!("match target out of bounds")
            }

            if mr.value >= 1 << 13 {
                bail!("match value out of bounds")
            }

            let odd = mr.target & 1 != 0;
            let idx = mr.target >> 1;
            let reg = &mut *m.matchregs.add(idx);
            match odd {
                true => {
                    reg.__bindgen_anon_1.set__u2(1);
                    reg.__bindgen_anon_1.set_m2(u32::from(mr.value));
                }
                false => {
                    reg.__bindgen_anon_1.set__u1(1);
                    reg.__bindgen_anon_1.set_m1(u32::from(mr.value));
                }
            }
        }

        assert!(self.quads.len() <= m.nquad as usize);
        for (i, q) in self.quads.iter().enumerate() {
            let out = &mut *m.insns.add(i);
            out.quad = std::mem::transmute_copy(&q.ops.words);
            out.seq = q.ops.seq.0;
        }

        Ok(())
    }
}

extern "C" {
    fn crypt_factor_patch(patch: *mut zt::patch_t) -> std::ffi::c_int;
}

fn real_main() -> Result<()> {
    let args = clap::builder::Command::new("uasm")
        .arg(clap::Arg::new("file").required(false))
        .arg(
            clap::Arg::new("template")
                .short('t')
                .long("template")
                .required(true)
                .value_parser(value_parser!(CString)),
        )
        .arg(
            clap::Arg::new("output")
                .short('o')
                .long("output")
                .required(true)
                .value_parser(value_parser!(CString)),
        )
        .arg(
            clap::Arg::new("resign")
                .long("resign")
                .action(clap::ArgAction::SetTrue),
        )
        .get_matches();
    let input = if let Some(path) = args.get_one::<String>("file") {
        Box::new(BufReader::new(std::fs::File::open(path)?)) as Box<dyn BufRead>
    } else {
        Box::new(std::io::stdin().lock()) as Box<dyn BufRead>
    };

    let mut assembler = Assembler::new();

    let mut lines = input.lines();
    let mut current_line = 0;
    while let Some(line) = lines.next().transpose()? {
        assembler
            .process_line(&line)
            .with_context(|| format!("While processing line {current_line}"))?;
        current_line += 1;
    }

    assembler.process_eof()?;

    let template = args.get_one::<CString>("template").unwrap();
    let output = args.get_one::<CString>("output").unwrap();
    let mut patch = unsafe {
        let patch = load_patch_file(template.as_ptr());
        NonNull::<zt::patch>::new(patch).context("Failed to open patch")?
    };

    unsafe {
        let patch_mut = patch.as_mut();
        println!("applying to template");
        println!("quad limit: {}", { patch_mut.nquad });
        println!("match limit: {}", 2 * patch_mut.nmatch);
        assembler.apply(patch)?;

        zt::crypt_patch_hash(patch_mut.hash.as_mut_ptr(), patch.as_ptr());

        if args.get_flag("resign") {
            println!("resigning patch");
            crypt_factor_patch(patch.as_ptr());
        }

        save_patch_file(patch.as_ptr(), output.as_ptr());
        free_patch_file(patch.as_ptr());
    }

    Ok(())
}

fn main() {
    real_main().unwrap();
}
