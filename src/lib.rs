// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Óscar García Amor <ogarcia@connectical.com>

//! Meshview Planet: a map of the nodes every Meshview instance sees.
//!
//! A library as well as a binary only so that the integration tests under
//! `tests/` can reach the pieces; nothing else is meant to depend on it.

pub mod admin;
pub mod app;
pub mod areas;
pub mod collector;
pub mod config;
pub mod db;
pub mod import;
pub mod meshview;
pub mod net;
pub mod notify;
pub mod submit;
pub mod term;
pub mod web;
