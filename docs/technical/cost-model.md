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

## Rebuild or keep

Demotion, compaction and reconstruction shrink the prompt and change it from
some token on. The provider's cache holds the prompt as a prefix, so everything
behind the first changed token is lost and the next request writes the new text.
`sven_cost::assess` compares, over the expected remaining requests, keeping the
prompt with rebuilding it (prompt side only; what both options share cancels):

- keep: the first request starts from the tokens the cache holds, each later
  one reads the whole prompt;
- rebuild: any model call the rebuild makes (a summary) at cold prices, the
  first request reads only the cache in front of the first changed token and
  writes the rest, each later one reads the smaller prompt.

`Assessment` carries both totals, their difference, the recommendation (`keep`,
`rebuild`, or `unknown` when a price is undeclared) and the break-even request
count. Worked example: a 100,000-token prompt, a 10,000-token demotion whose
first changed token is at 20,000, read 0.3 and write 3.75 per million: the
rebuild costs 0.2385 more on the first request and saves 0.003 on each later
one, so it breaks even at 80.5 requests and `keep` wins below that.

The remaining requests are declared, else about as many as the session has made
so far (capped); this is a heuristic, not a measurement.

`CacheAware` (`off`, `advise`, `enforce`) and `gate` turn an assessment into an
action. Only `enforce` skips, only a rebuild that is not needed to fit the
model's window, and only on a known comparison that favours keeping. Each
decision is a `RebuildRecord` in the request's ledger record under `cost`.

### Where it is wired

`cost.cache_aware` (`off` by default) turns the gate on for **demotion**
([context-assembly.md](context-assembly.md)). When a request's assembly would
demote results for the first time, `ContextAssembler::assemble_with` prices the
two prompts - the one with the new demotions undone (what the cache already
holds, plus the tokens kept) and the one with them - with the model's declared
price (`cost.prices` by `provider/model`, else the catalog's), the cache the
previous request is expected to have left (`CacheState::expected` from what it
reported reading and writing, and the idle time against the declared
time-to-live) and the requests still expected (`cost.remaining_requests`, else
as many as the session has made, capped at 200). `advise` records the comparison
and demotes anyway; `enforce` leaves the new demotions undone when they do not
break even. Demotions an earlier request sent are never undone, so the gate
cannot disturb the prefix the cache holds, and a demotion that is needed to get
under the window, or one whose price is undeclared, always goes ahead. A session's
first request has no cache to lose, so a demotion there pays whatever remains.

Compaction and reconstruction are **not** gated. Compaction fires when the
conversation reaches the window's threshold, that is when it is needed to fit; a
resumed session starts from a cold cache unless it was resumed within the
time-to-live, a case the gate does not weigh.

Configuration:

```yaml
cost:
  cache_aware: enforce          # off | advise | enforce
  remaining_requests: 40        # optional
  prices:
    "anthropic/claude-sonnet-4": # per million tokens, one currency of your choice
      input_per_mtok: 3.0
      output_per_mtok: 15.0
      cache: { read_per_mtok: 0.3, write_per_mtok: 3.75, ttl_secs: 300 }
```

A price that is not usable (negative, or a cache read above a cache write) is
reported and the gate stays off.
