#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CONTRACTS_DIR="$ROOT_DIR/contracts"
CONFIG_DIR="$ROOT_DIR/config"

RPC_URL="http://localhost:8545"
DEPLOYER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
OPERATOR_KEY="0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"
CHAIN_ID=31337

cd "$CONTRACTS_DIR"
OPERATOR_KEY="$OPERATOR_KEY" forge script script/Deploy.s.sol \
  --rpc-url "$RPC_URL" \
  --broadcast \
  --private-key "$DEPLOYER_KEY"

BROADCAST_FILE="$CONTRACTS_DIR/broadcast/Deploy.s.sol/$CHAIN_ID/run-latest.json"

TOKEN_ADDRESS=$(jq -r '.transactions[] | select(.contractName == "MockToken" and .transactionType == "CREATE") | .contractAddress' "$BROADCAST_FILE")
VAULT_ADDRESS=$(jq -r '.transactions[] | select(.contractName == "Vault" and .transactionType == "CREATE") | .contractAddress' "$BROADCAST_FILE")

mkdir -p "$CONFIG_DIR"
cat > "$CONFIG_DIR/local.json" <<EOF
{
  "rpc_url": "$RPC_URL",
  "ws_url": "ws://localhost:8545",
  "chain_id": $CHAIN_ID,
  "token_address": "$TOKEN_ADDRESS",
  "vault_address": "$VAULT_ADDRESS",
  "deployer_key": "$DEPLOYER_KEY",
  "operator_key": "$OPERATOR_KEY"
}
EOF

echo "Deployed MockToken at $TOKEN_ADDRESS"
echo "Deployed Vault at $VAULT_ADDRESS"
echo "Config written to $CONFIG_DIR/local.json"
