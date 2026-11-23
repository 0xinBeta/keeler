// Keeler's in-page wallet: an Ed25519 key in localStorage, exposed as a Wallet
// Standard wallet so the client treats it like any extension. For test tokens.
import {
  createKeyPairSignerFromPrivateKeyBytes,
  getTransactionDecoder,
  getTransactionEncoder,
  partiallySignTransaction,
  signBytes,
  type KeyPairSigner,
} from "@solana/kit";
import type { Wallet, WalletAccount } from "@wallet-standard/base";
import type {
  SolanaSignMessageFeature,
  SolanaSignMessageInput,
  SolanaSignTransactionFeature,
  SolanaSignTransactionInput,
} from "@solana/wallet-standard-features";
import type { StandardConnectFeature, StandardEventsFeature } from "@wallet-standard/features";

export const IN_PAGE_WALLET_NAME = "Keeler in-page wallet";
const STORAGE_KEY = "keeler:in-page-wallet:v1";
const CHAINS = ["solana:localnet", "solana:devnet"] as const;
const ICON =
  "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAzMiAzMiI+PGNpcmNsZSBjeD0iMTYiIGN5PSIxNiIgcj0iMTEiIGZpbGw9Im5vbmUiIHN0cm9rZT0iI0U5Qjg3MiIgc3Ryb2tlLXdpZHRoPSIzIi8+PC9zdmc+" as const;

const toHex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

/** The stored seed, created on first use. Throws if storage is unavailable. */
function seed(): Uint8Array {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored && /^[0-9a-f]{64}$/.test(stored)) return Uint8Array.from(stored.match(/../g)!, (b) => parseInt(b, 16));
  const fresh = crypto.getRandomValues(new Uint8Array(32));
  localStorage.setItem(STORAGE_KEY, toHex(fresh));
  if (localStorage.getItem(STORAGE_KEY) !== toHex(fresh)) throw new Error("could not save the in-page wallet key");
  return fresh;
}

export function createInPageWallet(): Wallet {
  let signer: Promise<KeyPairSigner> | undefined;
  let account: WalletAccount | undefined;
  const getSigner = () => (signer ??= createKeyPairSignerFromPrivateKeyBytes(seed()));
  const mine = (a: WalletAccount) => {
    if (!account || a.address !== account.address) throw new Error("unknown account");
  };

  const connect: StandardConnectFeature["standard:connect"]["connect"] = async () => {
    const s = await getSigner();
    account ??= {
      address: s.address,
      publicKey: new Uint8Array(await crypto.subtle.exportKey("raw", s.keyPair.publicKey)),
      chains: CHAINS,
      features: ["solana:signTransaction", "solana:signMessage"],
    };
    return { accounts: [account] };
  };
  const signMessage = async (...inputs: readonly SolanaSignMessageInput[]) => {
    const s = await getSigner();
    return Promise.all(
      inputs.map(async (i) => {
        mine(i.account);
        return { signedMessage: i.message, signature: new Uint8Array(await signBytes(s.keyPair.privateKey, i.message)) };
      }),
    );
  };
  const signTransaction = async (...inputs: readonly SolanaSignTransactionInput[]) => {
    const s = await getSigner();
    return Promise.all(
      inputs.map(async (i) => {
        mine(i.account);
        const tx = getTransactionDecoder().decode(i.transaction);
        const signed = await partiallySignTransaction([s.keyPair], tx);
        return { signedTransaction: new Uint8Array(getTransactionEncoder().encode(signed)) };
      }),
    );
  };

  const features: StandardConnectFeature & StandardEventsFeature & SolanaSignMessageFeature & SolanaSignTransactionFeature = {
    "standard:connect": { version: "1.0.0", connect },
    "standard:events": { version: "1.0.0", on: () => () => {} },
    "solana:signMessage": { version: "1.1.0", signMessage },
    "solana:signTransaction": { version: "1.0.0", supportedTransactionVersions: [1], signTransaction },
  };
  return {
    version: "1.0.0",
    name: IN_PAGE_WALLET_NAME,
    icon: ICON,
    chains: CHAINS,
    features,
    get accounts() {
      return account ? [account] : [];
    },
  };
}
