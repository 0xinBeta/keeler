// The network this site was built for (`VITE_NETWORK`). The client refuses a service on another
// network, and the safety rules that depend on the network read this, never the server's config.
/// <reference types="vite/client" />

export const BUILD_NETWORK: string | undefined = import.meta.env.VITE_NETWORK;

/** The browser test wallet holds its key in page storage: test networks only. */
export const testWalletAllowed = (network = BUILD_NETWORK) => network === "local" || network === "devnet";

/** The Wallet Standard chain id of a Keeler network name. */
export const walletChain = (network: string): `solana:${string}` => (network === "local" ? "solana:localnet" : `solana:${network}`);
