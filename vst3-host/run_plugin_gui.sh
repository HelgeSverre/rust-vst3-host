#!/bin/bash
# Script to run the plugin_gui example
cd "$(dirname "$0")"
cargo run --example plugin_gui --features cpal-backend --release