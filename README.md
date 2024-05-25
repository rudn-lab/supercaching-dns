# supercaching-dns
DNS caching server that will cache data for longer than allowed by the spec

## Why would you want this?

In some environments (such as corporate and university networks)
you are only allowed to use a single authorized DNS server, which may do things like
serving your internal domain's zone, or content filtering.
At times, this server becomes unreliable,
and will not respond to a query for a while;
this causes downstream issues, such as a process failing to reach
a domain name and therefore deciding that you're offline.

Most internet servers live for a long time, and will serve the right content
even if the DNS is changed to point away from them.
Additionally, large sites have a large number of origin servers,
and perform DNS-based round-robin/load-balancing,
so if you get an outdated response then it'll hit a relevant server.

In these cases, you may want to get some answer (even if outdated) instead of no answer at all.
This server records all responses it gets, and then doesn't delete them after their TTL expires,
so that it can serve them if the upstream server is unreliable at that particular moment.