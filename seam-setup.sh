#!/bin/bash
# Seam workspace and Igloohome setup script
# This script connects your Igloohome device to your Seam workspace

set -e

echo "=== Seam Setup for aloof-innkeep ==="
echo ""

# Check if .env exists
if [ ! -f ".env" ]; then
    echo "Error: .env file not found"
    exit 1
fi

# Load or prompt for Seam credentials
if grep -q "SEAM_API_KEY" .env; then
    SEAM_API_KEY=$(grep SEAM_API_KEY .env | cut -d= -f2 | xargs)
    echo "✓ Found existing SEAM_API_KEY in .env"
else
    echo "No SEAM_API_KEY found in .env"
    echo ""
    echo "Setup steps:"
    echo "1. Go to https://console.seam.co"
    echo "2. Sign up and create a workspace"
    echo "3. Go to Developer → API Keys"
    echo "4. Copy the Workspace API Key"
    echo ""
    
    read -p "Paste your Seam Workspace API Key: " SEAM_API_KEY
    
    if [ -z "$SEAM_API_KEY" ]; then
        echo "Error: API key required"
        exit 1
    fi
    
    # Add to .env
    echo "" >> .env
    echo "# Seam (Smart Lock) Configuration" >> .env
    echo "SEAM_API_KEY=$SEAM_API_KEY" >> .env
    echo "✓ Added SEAM_API_KEY to .env"
fi

echo ""
echo "=== Seam Device Configuration ==="
echo ""

# Check if SEAM_DEVICE_ID already exists in .env
if grep -q "SEAM_DEVICE_ID" .env; then
    DEVICE_ID=$(grep SEAM_DEVICE_ID .env | cut -d= -f2 | xargs)
    echo "✓ Found existing SEAM_DEVICE_ID in .env: $DEVICE_ID"
else
    echo "No SEAM_DEVICE_ID found in .env"
    echo ""
    echo "Setup steps:"
    echo "1. Go to https://console.seam.co"
    echo "2. Navigate to Devices"
    echo "3. Copy your device ID"
    echo ""
    
    read -p "Paste your Seam Device ID: " DEVICE_ID
    
    if [ -z "$DEVICE_ID" ]; then
        echo "Error: Device ID required"
        exit 1
    fi
    
    # Add to .env
    echo "SEAM_DEVICE_ID=$DEVICE_ID" >> .env
    echo "✓ Added SEAM_DEVICE_ID to .env"
fi

echo ""
echo "=== Setup Complete ==="
echo ""
echo "Your .env now has:"
echo "  - SEAM_API_KEY: ${SEAM_API_KEY:0:20}..."
echo "  - SEAM_DEVICE_ID: $DEVICE_ID"
echo ""
echo "Next steps:"
echo "1. cargo build --release"
echo "2. cargo run --release"
echo ""
echo "The sync will automatically create access codes for each Airbnb reservation."
