# Cost model

Sven decides what to send to a model by tokens. This page is about the other
half: what those tokens cost, and whether a decision that saves tokens saves
money. The two differ because of the provider's prompt cache. A rewrite of the
context that removes 10,000 tokens also throws away the cached prefix behind the
first changed token, and the next request pays to write the new prefix. A cheaper
model per token is not cheaper per step once the conversation has to be sent to
it again.

Everything here is **advisory and off by default**. A cost figure never
overrides a permission, an approval, a context-window limit or a data-egress
rule, and nothing in this page runs unless a `cost` or `routing` key turns it
on (see [Configuration](#configuration)). Where a price is not declared the
figure is `unknown`, and an unknown figure is never chosen.

## Where the inputs come from

| input | source |
|-------|--------|
| token counts of a request | the request ledger's estimate ([token-ledger.md](token-ledger.md)), reconciled with what the provider reported |
| cache hits and writes | the provider's own usage report of the previous request (`reported.cache_read`, `reported.cache_write`) |
| prices, cache prices, cache time-to-live | declared: the `cost.prices` configuration, else `sven-model-catalog`'s `prices.yaml`; a model with neither has an unknown price |
| time since the last request | the wall clock at the request boundary, owned by the executor |

`sven-model-catalog` ships `prices.yaml` **empty**. A price is a fact about a
contract and goes stale silently; declare the ones you pay. Prices are per
million tokens in one currency that the declarer chooses; sven does not convert
or label it.

## One step, term by term

`sven_cost::estimate` prices one step as `CostTerms`. The terms do not overlap, so
their sum is the cost and no token is charged twice.

| term | what it is |
|------|------------|
| `input_uncached` | tokens this step appends, at the input rate; the whole prompt when the provider has no cache |
| `cached_read` | prefix tokens the cache still holds, at the cache-read rate |
| `cache_write` | prefix tokens the cache no longer held (the time-to-live passed), written again at the cache-write rate |
| `output` | expected generated tokens, at the output rate |
| `handoff` | the whole prefix sent afresh to a *different* model, whose cache holds none of it |
| `recovery` | the declared probability that the step is redone, times what redoing it costs |

Each term is an `Amount`: a number, or the word `unknown` in JSON. Unknown is not
zero: it absorbs every sum it takes part in, `cheapest` never returns it, and a
model whose price is not declared is therefore never chosen on cost.

### Cache state

`CacheState::expected` treats the cache as all or nothing: the prefix is warm
when the last request was at most the time-to-live ago and cold otherwise. An
idle time that is not known counts as expired. When a previous request exists
`CacheState::after` uses what the provider reported it cached
(`cache_read + cache_write`) instead of an estimate.

### Assumptions and limits

- Cache expiry is a step function. Providers refresh a prefix's lifetime when
  it is used, and some cache in blocks; neither is modelled.
- Tokens are the ledger's estimate (`bytes_div_4`) unless the provider
  reported a figure; the residual of that estimate carries into every term.
- A price that is declared wrongly is wrong in every decision made from it.
- `cache_read_per_mtok` above `cache_write_per_mtok` is rejected: a cache hit
  priced above a miss is not a cache.
