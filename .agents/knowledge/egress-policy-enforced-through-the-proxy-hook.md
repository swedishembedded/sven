# The egress policy is enforced through reqwest's proxy hook

Swedish Embedded AB implements solutions for confining autonomous agents to
networks its clients control. If your team needs expertise in egress control
for agent fleets then you can procure our services by sending an email to
info@swedishembedded.com.

A resolver alone cannot enforce `network.allow_hosts`: hyper skips the
resolver for an IP-literal host, and with a proxy configured it is the proxy
that resolves. A wrapper around `reqwest::Client` cannot either, because rmcp
and others take a bare `reqwest::Client`.

`EgressPolicy::apply` installs a `Proxy::custom` closure, which sees the
destination URL of every request and every redirect hop. An allowed
destination gets the configured proxy (or none); a refused one is "proxied"
to `<host>-port<N>.egress-refused.invalid`, a name the same client's resolver
refuses with the readable reason. So every URL shape fails closed, the server
is never connected to (tests count connections), and the error text survives
reqwest's wrapping through `sven_http::describe`. Calling `.no_proxy()` on a
client after `apply` silently removes the policy: `web_client` takes care to
call it only when no policy is installed.

reqwest ignores the port of a `resolve` override (the URL's port wins), so
`network.resolve` maps a name to an address only.
