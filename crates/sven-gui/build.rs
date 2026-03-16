// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
fn main() {
    slint_build::compile("ui/main-window.slint").expect("Slint build failed");
}
