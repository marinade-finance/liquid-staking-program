#!/bin/bash
export BPF_FILE=target/verifiable/marinade_finance.so
echo "anchor build -v && prepare deploy buffer on mainnet-beta?"
read -p "Press any key to continue..."
set -e
anchor build -v
ls -l $BPF_FILE

echo ---------------------
echo solana program WRITE-BUFFER
echo ---------------------
solana program write-buffer -v -u https://marinade.rpcpool.com $BPF_FILE > temp/buffer.txt
export BUFFER_ADDRESS=$(cat temp/buffer.txt | sed -n 's/.*Buffer://p')
echo solana program show $BUFFER_ADDRESS
echo after verifying do set-buffer-authority to move buffer auth to the multisig
echo solana program set-buffer-authority --new-buffer-authority 551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp $BUFFER_ADDRESS
#solana program set-buffer-authority --new-buffer-authority 551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp $BUFFER_ADDRESS
echo use https://aankor.github.io/multisig-ui/#/magrsHFQxkkioAy45VWnZnFBBdKVdy2ZiRoRGYT9Wed to create mulisig-upgrade txn
echo program_id: MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
echo buffer: $BUFFER_ADDRESS
