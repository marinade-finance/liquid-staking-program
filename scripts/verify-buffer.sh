set -ex
cd programs/marinade-finance
anchor verify $1 --provider.cluster mainnet
cd ../..
