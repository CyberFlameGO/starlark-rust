/*
 * Copyright 2018 The Starlark in Rust Authors.
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Bytecode generation tests.

mod and_or;
mod compr;
mod expr;
mod if_stmt;

use crate::{
    assert::Assert,
    eval::{bc::opcode::BcOpcode, FrozenDef},
};

pub(crate) fn test_instrs(expected: &[BcOpcode], def_program: &str) {
    let mut a = Assert::new();
    let def = a
        .module("instrs.star", def_program)
        .get("test")
        .unwrap()
        .downcast::<FrozenDef>()
        .unwrap();
    let mut opcodes = def.bc().instrs.opcodes();
    assert_eq!(Some(BcOpcode::End), opcodes.pop());
    assert_eq!(expected, opcodes);
}
