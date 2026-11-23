// The swap client behind the `Keeler` interface. Every "now" is chain time.
import {
  address,
  getBase64EncodedWireTransaction,
  getSignatureFromTransaction,
  getTransactionDecoder,
  getTransactionEncoder,
  type Address,
  type Transaction,
} from "@solana/kit";
import {
  LocalKeys,
  SOL_MINT,
  Wallet as SprWallet,
  buildDepositTransaction,
  buildRegistrationTransaction,
  buildWithdrawalTransaction,
  initializePoseidon,
  syncWallet,
} from "@heliuslabs/spr";
import { SignedTeePolicy, SprClient } from "@heliuslabs/spr/client";
import { ed25519DerivationMessage } from "@heliuslabs/spr/keypair";
import { getAssociatedTokenAddress } from "@heliuslabs/spr/addresses";
import { backfillAssetRegistry, fetchUserRecord } from "@heliuslabs/spr/wallet";
import { getWallets } from "@wallet-standard/app";
import type { Wallet, WalletAccount } from "@wallet-standard/base";
import type { SolanaSignMessageFeature, SolanaSignTransactionFeature } from "@solana/wallet-standard-features";
import type { StandardConnectFeature } from "@wallet-standard/features";
import {
  KeelerError,
  type Asset,
  type Balance,
  type Config,
  type Keeler,
  type Order,
  type OrderStatus,
  type Quote,
  type SwapStep,
} from "./index";
import { fairBuyAmount, ttlOk, vsFairBps, withinBand, type Price } from "./price";
import { orderStore, type OrderRecord } from "./storage";
import { buildCancel, buildMake, leafIndex, orderStatus, toHex, type SwapContext } from "./swap";
import { createInPageWallet, IN_PAGE_WALLET_NAME } from "./wallet";

type ServerConfig = Config & {
  rpcPath: string;
  sprPath: string;
  reference: Record<string, { price: string | number; expo: number }>;
};
type QuoteReply = { dealer: string; quote: { buy_amount: string; expiry: number } | null }[];

const CLOCK_SYSVAR = address("SysvarC1ock11111111111111111111111111111111");
const TEE_POLICY_ORIGIN = "https://d3gbdb0egjwcw9.cloudfront.net";
const REQUIRED = ["standard:connect", "solana:signMessage", "solana:signTransaction"] as const;

const err = (code: KeelerError["code"], message: string) => new KeelerError(code, message);

function isRejection(e: unknown): boolean {
  const text = String((e as { message?: unknown })?.message ?? e).toLowerCase();
  return text.includes("reject") || text.includes("denied") || text.includes("cancel");
}

/** Rewraps anything that is not already a KeelerError. */
async function as<T>(code: KeelerError["code"], what: string, run: () => Promise<T>): Promise<T> {
  try {
    return await run();
  } catch (e) {
    if (e instanceof KeelerError) throw e;
    const detail = e instanceof Error ? e.message : String(e);
    throw err(code, `${what} failed: ${detail}`);
  }
}

async function getJson<T>(path: string, init?: RequestInit): Promise<T> {
  let res: Response;
  try {
    res = await fetch(path, init);
  } catch {
    throw err("network", `Could not reach ${path}. Check your connection and try again.`);
  }
  if (res.status === 429) throw err("rate-limited", "Too many requests. Wait a minute and try again.");
  if (!res.ok) throw err("network", `${path} answered HTTP ${res.status}. Try again shortly.`);
  return (await res.json()) as T;
}

export function createKeeler(): Keeler {
  let cfg: Promise<ServerConfig> | undefined;
  let sdk: Promise<SprClient> | undefined;
  let inPage: Wallet | undefined;
  let wallet: Wallet | undefined;
  let account: WalletAccount | undefined;
  let unlocked: { keys: LocalKeys; spr: SprWallet } | undefined;
  let busy = false;

  const config = () =>
    (cfg ??= getJson<ServerConfig>("/api/config").catch((e) => {
      cfg = undefined;
      throw e;
    }));

  const client = () =>
    (sdk ??= (async () => {
      const c = await config();
      await initializePoseidon();
      const origin = globalThis.location.origin;
      const spr = new URL(c.sprPath, origin);
      const local = c.network === "local";
      return new SprClient({
        solanaRpcUrl: new URL(c.rpcPath, origin),
        indexerUrl: spr,
        proverUrl: spr,
        // The local prover has no indexer of its own to read proof data from.
        ...(local ? { proofDataSource: "client" as const } : { proverTee: SignedTeePolicy.forNetwork(c.network as "devnet") }),
        // The page may only talk to its own origin: the Worker serves the TEE policy.
        fetch: (input, init) => {
          const url = String(input instanceof Request ? input.url : input);
          return fetch(url.startsWith(TEE_POLICY_ORIGIN) ? `${origin}/api/tee-policy` : input, init);
        },
      });
    })().catch((e) => {
      sdk = undefined;
      throw e;
    }));

  const owner = (): Address => {
    if (!account) throw err("wallet-unsupported", "Connect a wallet first.");
    return address(account.address);
  };
  const keys = () => {
    if (!unlocked) throw err("not-registered", "Unlock first: sign the key derivation message.");
    return unlocked;
  };
  const store = async () => orderStore((await config()).network, owner());

  const chainNow = async (): Promise<number> => {
    const acc = await as("network", "Reading chain time", async () => (await client()).getAccount(CLOCK_SYSVAR));
    if (!acc || acc.data.length < 40) throw err("network", "The chain returned no clock. Try again.");
    return Number(new DataView(acc.data.buffer, acc.data.byteOffset).getBigInt64(32, true));
  };

  /** Has the wallet sign `tx`, sends it, and waits for confirmation; returns the slot. */
  const signAndSend = async (tx: Transaction): Promise<bigint> => {
    const feature = (wallet!.features as SolanaSignTransactionFeature)["solana:signTransaction"];
    const net = (await config()).network;
    let signed: Transaction;
    try {
      const [out] = await feature.signTransaction({
        account: account!,
        transaction: new Uint8Array(getTransactionEncoder().encode(tx)),
        chain: `solana:${net}`,
      });
      signed = getTransactionDecoder().decode(out!.signedTransaction);
    } catch (e) {
      if (isRejection(e)) throw err("wallet-rejected", "You declined the transaction in your wallet. Nothing was sent.");
      throw err("wallet-unsupported", `The wallet could not sign this transaction (${String(e)}). Try Keeler's in-page wallet.`);
    }
    const c = await client();
    return as("network", "Sending the transaction", async () => {
      await c.solanaRpc.sendTransaction(getBase64EncodedWireTransaction(signed), { encoding: "base64" }).send();
      return c.confirmTransaction(getSignatureFromTransaction(signed));
    });
  };

  const ctx = async (): Promise<SwapContext> => {
    const k = keys();
    return { client: await client(), wallet: k.spr, keys: k.keys, payer: owner(), programId: address((await config()).programId) };
  };

  const sync = async (requireSlot?: bigint) => {
    const k = keys();
    await as("network", "Reading your private balance", async () =>
      syncWallet({ client: await client(), wallet: k.spr, keys: k.keys, ...(requireSlot ? { config: { requireSlot } } : {}) }),
    );
  };

  /** Unspent default-ring notes of `mint`. */
  const notes = (mint: Address) =>
    keys().spr.utxos().filter((u) => !u.spent && u.utxo.asset === mint && u.utxo.ringProgramId === undefined);

  const price = (c: ServerConfig, mint: string): Price => {
    const p = c.reference[mint];
    if (!p) throw err("network", `No reference price for ${mint}. The service is misconfigured.`);
    return { price: BigInt(p.price), expo: p.expo };
  };

  const fair = (c: ServerConfig, sell: Asset, buy: Asset, amount: bigint) => {
    const f = fairBuyAmount(amount, price(c, sell.mint), price(c, buy.mint), sell.decimals, buy.decimals);
    if (f === undefined) throw err("no-quotes", `${amount} ${sell.symbol} is too small to price in ${buy.symbol}.`);
    return f;
  };

  const askDealers = async (sell: Asset, buy: Asset, sellAmount: bigint): Promise<QuoteReply> => {
    const body = { sell_mint: sell.mint, buy_mint: buy.mint, sell_amount: sellAmount.toString(), trader: owner() };
    return getJson<QuoteReply>("/api/quotes", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  };

  const toQuote = (c: ServerConfig, f: bigint, sell: Asset, buy: Asset, sellAmount: bigint, r: QuoteReply[number]): Quote | undefined => {
    if (!r.quote || !/^\d+$/.test(r.quote.buy_amount) || !Number.isSafeInteger(r.quote.expiry)) return undefined;
    const buyAmount = BigInt(r.quote.buy_amount);
    return {
      dealer: r.dealer,
      sell,
      buy,
      sellAmount,
      buyAmount,
      expiry: r.quote.expiry,
      inBand: withinBand(buyAmount, f, c.bandBps),
      vsFairBps: vsFairBps(buyAmount, f),
    };
  };

  const toOrder = (r: OrderRecord): Order => ({
    id: r.id,
    sell: r.sell,
    buy: r.buy,
    sellAmount: r.sellAmount,
    buyAmount: r.buyAmount,
    expiry: r.expiry,
    status: r.status,
    ...(r.error ? { error: r.error } : {}),
  });

  /** Reads one record's status from its nullifier and chain time; saves a change. */
  const refresh = async (r: OrderRecord, now: number): Promise<OrderRecord> => {
    if (r.status === "settled" || r.status === "refunded" || r.status === "failed") return r;
    const c = await ctx();
    let status: OrderStatus = r.status;
    if ((await leafIndex(c, r.opening)) === undefined) {
      // Never landed, or not indexed yet: a pending order past expiry is not on chain.
      if (r.status === "pending" && now > r.expiry) status = "failed";
    } else {
      const s = await orderStatus(c, r.opening, { mint: address(r.buy.mint), amount: r.buyAmount });
      status = s === "settled" ? "settled" : s === "cancelled" ? "refunded" : now > r.expiry ? "refundable" : "locked";
    }
    if (status === r.status) return r;
    const next = { ...r, status, ...(status === "failed" ? { error: "The order never reached the chain; nothing is locked." } : {}) };
    (await store()).put(next);
    return next;
  };

  const exclusive = async <T>(run: () => Promise<T>): Promise<T> => {
    if (busy) throw err("busy", "Another swap or refund is in progress. Wait for it to finish.");
    busy = true;
    try {
      return await run();
    } finally {
      busy = false;
    }
  };

  const keeler: Keeler = {
    config: async () => {
      const { rpcPath: _r, sprPath: _s, reference: _p, ...rest } = await config();
      return rest;
    },

    wallets() {
      inPage ??= createInPageWallet();
      const installed = getWallets()
        .get()
        .filter((w) => w.name !== IN_PAGE_WALLET_NAME && REQUIRED.every((f) => f in w.features));
      return [...installed, inPage];
    },

    async connect(w) {
      if (!REQUIRED.every((f) => f in w.features)) {
        throw err("wallet-unsupported", `${w.name} cannot sign messages and transactions. Choose another wallet.`);
      }
      const versions = (w.features as SolanaSignTransactionFeature)["solana:signTransaction"].supportedTransactionVersions;
      if (!versions.includes(1)) {
        throw err("wallet-unsupported", `${w.name} cannot sign version 1 transactions, which private swaps need. Use Keeler's in-page wallet.`);
      }
      let accounts: readonly WalletAccount[];
      try {
        ({ accounts } = await (w.features as StandardConnectFeature)["standard:connect"].connect());
      } catch (e) {
        throw isRejection(e)
          ? err("wallet-rejected", `You declined the connection in ${w.name}.`)
          : err("wallet-unsupported", `${w.name} did not connect: ${String(e)}`);
      }
      const first = accounts[0];
      if (!first) throw err("wallet-rejected", `${w.name} shared no account.`);
      unlocked?.keys.destroy();
      unlocked = undefined;
      wallet = w;
      account = first;
      return { address: first.address };
    },

    async unlock() {
      const me = owner();
      const sign = (wallet!.features as SolanaSignMessageFeature)["solana:signMessage"];
      const message = ed25519DerivationMessage(new Uint8Array(account!.publicKey) as never);
      const once = async () => {
        try {
          const [out] = await sign.signMessage({ account: account!, message });
          return new Uint8Array(out!.signature);
        } catch (e) {
          if (isRejection(e)) throw err("wallet-rejected", "You declined the signature. Your private keys were not derived.");
          throw err("wallet-unsupported", `The wallet could not sign the key message (${String(e)}).`);
        }
      };
      // Keys derived from a signature that changes between calls could not be found again.
      const a = await once();
      const b = await once();
      try {
        if (a.length !== 64 || toHex(a) !== toHex(b)) {
          throw err("wallet-unsupported", "This wallet's signatures differ each time, so it cannot hold private keys. Use another wallet.");
        }
        const c = await client();
        const derived = LocalKeys.fromDerivationSeed({ solanaPublicKey: me, derivationSeed: a }, c.proofService);
        const address = derived.address();
        const record = await as("network", "Reading the user registry", () => fetchUserRecord({ rpc: c, owner: me }));
        if (
          record &&
          (toHex(record.nullifierPublicKey) !== toHex(address.nullifierPublicKey) ||
            toHex(record.viewingPublicKey) !== toHex(address.viewingPublicKey.toBytes()))
        ) {
          derived.destroy();
          throw err("keys-mismatch", "These keys differ from the ones registered for this wallet. Use the wallet that registered them.");
        }
        const spr = new SprWallet({ identity: address });
        await as("network", "Reading the asset registry", () => backfillAssetRegistry(spr, c));
        unlocked?.keys.destroy();
        unlocked = { keys: derived, spr };
      } finally {
        a.fill(0);
        b.fill(0);
      }
    },

    async isRegistered() {
      const c = await client();
      return as("network", "Reading the user registry", async () => (await fetchUserRecord({ rpc: c, owner: owner() })) !== undefined);
    },

    async register() {
      const c = await client();
      const tx = await as("network", "Building the registration", () =>
        buildRegistrationTransaction({ client: c, owner: owner(), address: keys().keys.address() }),
      );
      if (tx) await signAndSend(tx);
    },

    async balances(): Promise<Balance[]> {
      const c = await client();
      const cf = await config();
      await sync();
      const me = owner();
      return Promise.all(
        cf.assets.map(async (asset) => {
          const mint = address(asset.mint);
          const pub = await as("network", "Reading your public balance", async () => {
            if (mint === SOL_MINT) return (await c.solanaRpc.getBalance(me).send()).value;
            const ata = await getAssociatedTokenAddress(me, mint);
            try {
              return BigInt((await c.solanaRpc.getTokenAccountBalance(ata).send()).value.amount);
            } catch {
              return 0n; // No token account yet.
            }
          });
          const amounts = notes(mint).map((u) => u.utxo.amount);
          return {
            asset,
            public: pub,
            private: amounts.reduce((s, x) => s + x, 0n),
            largestNote: amounts.reduce((m, x) => (x > m ? x : m), 0n),
          };
        }),
      );
    },

    async deposit(asset, amount) {
      const c = await client();
      const me = owner();
      const mint = address(asset.mint);
      const tx = await as("network", "Building the deposit", async () =>
        buildDepositTransaction({
          client: c,
          feePayer: me,
          recipient: keys().keys.address(),
          amount,
          ...(mint === SOL_MINT ? {} : { asset: mint, splTokenAccount: await getAssociatedTokenAddress(me, mint) }),
        }),
      );
      await sync(await signAndSend(tx));
    },

    async withdraw(asset, amount) {
      const c = await client();
      const k = keys();
      await sync();
      const mint = address(asset.mint);
      const tx = await as("network", "Building the withdrawal", () =>
        buildWithdrawalTransaction({
          client: c,
          wallet: k.spr,
          keys: k.keys,
          feePayer: owner(),
          recipient: owner(),
          amount,
          ...(mint === SOL_MINT ? {} : { asset: mint }),
        }),
      );
      await sync(await signAndSend(tx));
    },

    async quotes(sell, buy, sellAmount) {
      const c = await config();
      const f = fair(c, sell, buy, sellAmount);
      const replies = await askDealers(sell, buy, sellAmount);
      const now = await chainNow();
      const quotes = replies
        .map((r) => toQuote(c, f, sell, buy, sellAmount, r))
        .filter((q): q is Quote => q !== undefined && ttlOk(q.expiry, now, c.minTtlSecs, c.maxTtlSecs));
      if (quotes.length === 0) {
        throw err("no-quotes", "No dealer quoted this swap. The dealers may be offline or out of inventory; try again later.");
      }
      // Best first: in band before out of band, then the larger amount; dealer order breaks ties.
      quotes.sort((a, b) => Number(b.inBand) - Number(a.inBand) || (b.buyAmount > a.buyAmount ? 1 : b.buyAmount < a.buyAmount ? -1 : 0));
      return { fair: f, quotes };
    },

    swap: (shown, on) =>
      exclusive(async () => {
        on("checking");
        const c = await config();
        const me = owner();
        keys();
        if (!c.dealers.some((d) => d.pubkey === shown.dealer)) throw err("no-quotes", "That dealer is not one of this service's dealers.");
        // A quote is only as good as the dealer's answer now: ask again at click.
        const replies = await askDealers(shown.sell, shown.buy, shown.sellAmount);
        const f = fair(c, shown.sell, shown.buy, shown.sellAmount);
        const reply = replies.find((r) => r.dealer === shown.dealer);
        const q = reply && toQuote(c, f, shown.sell, shown.buy, shown.sellAmount, reply);
        if (!q) throw err("quote-expired", "The dealer no longer quotes this swap. Ask for new quotes.");
        if (q.buyAmount < shown.buyAmount) {
          throw err("quote-expired", "The dealer's price got worse since you saw it. Ask for new quotes and review them.");
        }
        if (!q.inBand) throw err("out-of-band", `The quote is more than ${c.bandBps / 100}% from the reference price. Choose another quote.`);
        const now = await chainNow();
        if (!ttlOk(q.expiry, now, c.minTtlSecs, c.maxTtlSecs)) {
          throw err("quote-expired", `The quote has under ${c.minTtlSecs} s left (or too long a lock). Ask for new quotes.`);
        }
        if (!(await keeler.isRegistered())) throw err("not-registered", "Register your private wallet before swapping.");
        await sync();
        // `make` spends one note, so one note must cover the whole amount.
        const source = notes(address(q.sell.mint))
          .filter((u) => u.utxo.amount >= q.sellAmount)
          .sort((a, b) => (a.utxo.amount < b.utxo.amount ? -1 : 1))[0];
        if (!source) {
          throw err("insufficient-note", `No single private note of ${q.sell.symbol} covers this amount. Lower it to your largest note.`);
        }

        const built = await as("proof", "Proving the order", async () =>
          buildMake(
            await ctx(),
            { source, sellAmount: q.sellAmount, buyMint: address(q.buy.mint), buyAmount: q.buyAmount, expiry: BigInt(q.expiry), dealer: address(q.dealer) },
            () => on("proving"),
          ),
        );
        const record: OrderRecord = {
          id: toHex(built.opening.orderHash),
          dealer: q.dealer,
          sell: q.sell,
          buy: q.buy,
          sellAmount: q.sellAmount,
          buyAmount: q.buyAmount,
          expiry: q.expiry,
          status: "pending",
          createdAt: Date.now(),
          opening: built.opening,
        };
        const orders = await store();
        // Without this record the browser could never refund the order: no record, no send.
        try {
          orders.put(record);
        } catch (e) {
          throw err("storage", `Could not save the order in this browser (${String(e)}), so it was not sent. Free some site storage and try again.`);
        }
        on("signing");
        try {
          await signAndSend(built.transaction);
        } catch (e) {
          if (e instanceof KeelerError && (e.code === "wallet-rejected" || e.code === "wallet-unsupported")) {
            orders.put({ ...record, status: "failed", error: e.message });
            throw e;
          }
          const message = `${e instanceof Error ? e.message : String(e)} The order may be locked; refund it after ${new Date(q.expiry * 1000).toLocaleTimeString()}.`;
          orders.put({ ...record, error: message });
          throw err("network", message);
        }
        const locked = { ...record, status: "locked" as const };
        orders.put(locked);
        on("locked");
        return toOrder(locked);
      }),

    async orders() {
      const now = await chainNow();
      const all = (await store()).all();
      return Promise.all(all.map(async (r) => toOrder(await refresh(r, now).catch(() => r))));
    },

    refund: (orderId) =>
      exclusive(async () => {
        const orders = await store();
        const found = orders.get(orderId);
        if (!found) throw err("storage", "This browser has no record of that order.");
        const now = await chainNow();
        const r = await refresh(found, now);
        if (r.status !== "refundable") {
          throw err("quote-expired", r.status === "locked" ? `The order can be refunded after its expiry, ${new Date(r.expiry * 1000).toLocaleTimeString()}.` : `The order is ${r.status}; there is nothing to refund.`);
        }
        const tx = await as("proof", "Proving the refund", async () => buildCancel(await ctx(), r.opening, () => {}));
        try {
          await signAndSend(tx);
        } finally {
          // The nullifier, not the send result, says what happened: a dealer may have settled first.
          await refresh(r, await chainNow()).catch(() => r);
        }
        await sync();
      }),

    async faucet() {
      const c = await config();
      if (!c.faucet) throw err("network", "This network has no faucet.");
      const res = await getJson<{ ok?: boolean; error?: string }>("/api/faucet", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ wallet: owner() }),
      });
      if (!res.ok) throw err("rate-limited", `The faucet refused: ${res.error ?? "unknown reason"}. Try again tomorrow.`);
    },

    chainNow,
  };
  return keeler;
}

