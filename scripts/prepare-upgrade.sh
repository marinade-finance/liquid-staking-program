#!/bin/bash
export BPF_FILE=target/deploy/marinade_finance.so
echo --------
solana config get
echo --------
echo "remember to set SOLANA CONFIG pointing to A RELIABLE MAINNET RPC: solana config set -u [rpc_url]"
read -p "Press any key to continue..."
echo "solana-verify build (SBPF v3) && prepare deploy buffer on mainnet-beta?"
read -p "Press any key to continue..."
set -e
if [ $1. = . ]
then
    solana-verify build --arch v3 --base-image solanafoundation/solana-verifiable-build@sha256:12fd4c0a0790f0fc41ef74b0cdb6bccc167ba6137eb1adb749bac649481c86bd --library-name marinade_finance $PWD
elif [ $1. = "--skip-build." ]
then
    echo "skip solana-verify build"
else
    echo "invalid arguemnt: $1"
    exit 1
fi
ls -l $BPF_FILE

echo ---------------------
echo solana program WRITE-BUFFER
echo ---------------------
solana program write-buffer -v $BPF_FILE >temp/buffer.txt
export BUFFER_ADDRESS=$(cat temp/buffer.txt | sed -n 's/.*Buffer://p')
echo solana program show $BUFFER_ADDRESS
echo after verifying do set-buffer-authority to move buffer auth to the multisig
echo solana program set-buffer-authority --new-buffer-authority 551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp $BUFFER_ADDRESS
#solana program set-buffer-authority --new-buffer-authority 551FBXSXdhcRDDkdcb3ThDRg84Mwe5Zs6YjJ1EEoyzBp $BUFFER_ADDRESS
echo use https://marinade-finance.github.io/multisig-ui/#/magrsHFQxkkioAy45VWnZnFBBdKVdy2ZiRoRGYT9Wed to create mulisig-upgrade txn
echo program_id: MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
echo buffer: $BUFFER_ADDRESS
