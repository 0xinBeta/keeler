// The quiet part of the page: how a swap works, what is public, the limits.

export function Info() {
  return (
    <>
      <section className="section" id="how" aria-labelledby="how-title">
        <h2 id="how-title">How a swap works</h2>
        <ol className="cards">
          <li className="card">
            <svg viewBox="0 0 240 64" className="diagram" aria-hidden="true">
              <rect x="70" y="18" width="100" height="28" rx="4" className="d-band" />
              <line x1="120" y1="10" x2="120" y2="54" className="d-ref" />
              <circle cx="132" cy="32" r="5" className="d-ok" />
              <circle cx="108" cy="32" r="5" className="d-ok" />
              <circle cx="32" cy="32" r="5" className="d-off" />
            </svg>
            <h3>
              <span className="step-n">1</span> Ask the dealers
            </h3>
            <p>
              Keeler asks every dealer for a price. Any quote too far from the reference price is dropped, so a dealer
              cannot win with a bad price.
            </p>
          </li>
          <li className="card">
            <svg viewBox="0 0 240 64" className="diagram" aria-hidden="true">
              <rect x="96" y="26" width="48" height="30" rx="5" className="d-lock" />
              <path d="M106 26v-8a14 14 0 0 1 28 0v8" className="d-line" />
              <circle cx="120" cy="41" r="3.5" className="d-ok" />
            </svg>
            <h3>
              <span className="step-n">2</span> Lock a private order
            </h3>
            <p>
              Your browser proves the order and your wallet signs it. Your funds are locked for the best quote; nobody
              watching the chain sees the assets or amounts.
            </p>
          </li>
          <li className="card">
            <svg viewBox="0 0 240 64" className="diagram" aria-hidden="true">
              <path d="M40 22h120" className="d-line" />
              <path d="M150 14l10 8-10 8" className="d-line" />
              <path d="M200 44H80" className="d-line d-dim" />
              <path d="M90 36l-10 8 10 8" className="d-line d-dim" />
            </svg>
            <h3>
              <span className="step-n">3</span> Settle, or take it back
            </h3>
            <p>
              The dealer settles privately and you receive the other asset. If they do not settle before the quote
              expires, you take your funds back.
            </p>
          </li>
        </ol>
      </section>

      <section className="section" id="privacy" aria-labelledby="privacy-title">
        <h2 id="privacy-title">What is public and what is private</h2>
        <div className="table-wrap">
          <table className="plain">
            <thead>
              <tr>
                <th scope="col">Public, anyone can see</th>
                <th scope="col">Private, only you and the dealer</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td>Your wallet address and the dealer's</td>
                <td>Which assets you swap</td>
              </tr>
              <tr>
                <td>That a swap happened</td>
                <td>The amounts</td>
              </tr>
              <tr>
                <td>When it happened</td>
                <td>The price</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <section className="section" id="limits" aria-labelledby="limits-title">
        <h2 id="limits-title">Limits of this version</h2>
        <ul className="limits">
          <li>
            <strong>Test tokens only.</strong> tUSD and tSOL have no value. Do not send real funds.
          </li>
          <li>
            <strong>Unaudited.</strong> Neither the program nor its circuits have been reviewed by an outside auditor.
          </li>
          <li>
            <strong>One dealer.</strong> A single dealer runs on one machine, so quotes may be missing at times.
          </li>
          <li>
            <strong>Orders are remembered by this browser only.</strong> If you clear site data before an order
            expires, this page can no longer take your funds back.
          </li>
          <li>
            <strong>Prices and order status come through Keeler's server.</strong> You trust it for the reference price
            and for what it reads from the chain.
          </li>
        </ul>
      </section>
    </>
  );
}
