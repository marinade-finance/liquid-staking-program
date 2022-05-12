#!/bin/bash
export BPF_FILE=target/verifiable/marinade_finance.so
echo --------
solana config get
echo --------
echo "remember to set SOLANA CONFIG pointing to A RELIABLE MAINNET RPC: solana config set -u [rpc_url]"
read -p "Press any key to continue..."
echo "anchor build -v && prepare deploy buffer on mainnet-beta?"
read -p "Press any key to continue..."
set -e
if [ $1. = . ]
then
    anchor build -v
elif [ $1. = "--skip-build." ]
then
    echo "skip anchor build -v"
else
    echo "invalid arguemnt: $1"
    exit 1
fi
ls -l $BPF_FILE

echo ---------------------
echo solana program WRITE-BUFFER
echo ---------------------
# note: solana v1.10 is struggling to write-buffer 1017 txs, we use a patched CLI with extended retries
~/repos/solana/solana/target/debug/solana program write-buffer -v $BPF_FILE >temp/buffer.txt
#solana program --skip-fee-check write-buffer -v $BPF_FILE --commitment processed >temp/buffer.txt
export BUFFER_ADDRESS=$(cat temp/buffer.txt | sed -n 's/.*Buffer://p')
echo solana program show $BUFFER_ADDRESS
echo after verifying do set-buffer-authority to move buffer auth to the multisig
echo solana program set-buffer-authority --new-buffer-authority 551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp $BUFFER_ADDRESS
#solana program set-buffer-authority --new-buffer-authority 551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp $BUFFER_ADDRESS
echo use https://aankor.github.io/multisig-ui/#/magrsHFQxkkioAy45VWnZnFBBdKVdy2ZiRoRGYT9Wed to create mulisig-upgrade txn
echo program_id: MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
echo buffer: $BUFFER_ADDRESS
