// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

use move_command_line_common::files::verify_and_create_named_address_mapping;
use move_compiler::{
    command_line::{self as cli},
    shared::{self, Flags, NumericalAddress},
};
use structopt::*;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "Move Check",
    about = "Check Move source code, without compiling to bytecode."
)]
pub struct Options {
    /// The source files to check
    #[structopt(name = "PATH_TO_SOURCE_FILE")]
    pub source_files: Vec<String>,

    /// The library files needed as dependencies
    #[structopt(
        name = "PATH_TO_DEPENDENCY_FILE",
        short = cli::DEPENDENCY_SHORT,
        long = cli::DEPENDENCY,
    )]
    pub dependencies: Vec<String>,

    /// The output directory for saved artifacts, namely any 'move' interface files generated from
    /// 'mv' files
    #[structopt(
        name = "PATH_TO_OUTPUT_DIRECTORY",
        short = cli::OUT_DIR_SHORT,
        long = cli::OUT_DIR,
    )]
    pub out_dir: Option<String>,

    /// Named address mapping
    #[structopt(
        name = "NAMED_ADDRESSES",
        short = "a",
        long = "addresses",
        parse(try_from_str = shared::parse_named_address)
    )]
    pub named_addresses: Vec<(String, NumericalAddress)>,

    #[structopt(flatten)]
    pub flags: Flags,
}

pub fn main() -> anyhow::Result<()> {
    let Options {
        source_files,
        dependencies,
        out_dir,
        flags,
        named_addresses,
    } = Options::from_args();
    let named_addr_map = verify_and_create_named_address_mapping(named_addresses)?;
    let _files = move_compiler::Compiler::new(
        vec![(source_files, named_addr_map.clone())],
        vec![(dependencies, named_addr_map)],
    )
    .set_interface_files_dir_opt(out_dir)
    .set_flags(flags)
    .check_and_report()?;
    Ok(())
}
