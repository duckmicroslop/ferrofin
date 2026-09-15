# Ferrofin Helm chart

Official chart for the [Ferrofin](https://github.com/mangoleaf/ferrofin) media server — a Rust implementation of the Jellyfin server API.

The value contract mirrors the common subset of the upstream
[jellyfin-helm](https://github.com/jellyfin/jellyfin-helm) chart, so moving a Jellyfin
release to Ferrofin is near-zero churn. Two intentional differences:

- the app config block is `ferrofin:` (not `jellyfin:`), with no `enableDLNA` (Ferrofin has no DLNA);
- config mounts at `/data` (Ferrofin's data dir), not `/config`.

## Install

The chart is published as an OCI artifact next to the image:

```bash
helm install ferrofin oci://ghcr.io/mangoleaf/ferrofin/charts/ferrofin \
  --version <chart-version> -n ferrofin --create-namespace -f my-values.yaml
```

The chart version equals the Ferrofin release it ships (`v1.2.3` → chart `1.2.3`). The
image is public on GHCR, so no pull secret is needed; if you mirror it into a private
registry, set `image.repository` and `imagePullSecrets` in your values.

## Key values

| Key | Default | Purpose |
|---|---|---|
| `image.repository` / `image.tag` | see `values.yaml` / chart appVersion | Server image |
| `imagePullSecrets` | `[]` | Secrets for the private registry |
| `discovery.enabled` | `false` | Declare UDP 7359 and create a separate discovery Service; allow it through the chart NetworkPolicy |
| `discovery.service.type` / `.annotations` | `ClusterIP` / `{}` | UDP Service type and provider annotations |
| `hostNetwork` | `false` | Share node networking for LAN broadcasts; requires `strategy.type: Recreate` |
| `service.port` | `8096` | Port Ferrofin listens on (also the container port) |
| `persistence.config.enabled` | `true` | Persist the data dir; `false` → emptyDir |
| `persistence.config.mountPath` | `/data` | Where the data dir mounts |
| `persistence.config.existingClaim` | `""` | Use an existing PVC instead of a chart-created one |
| `volumes` / `volumeMounts` | `[]` | Extra media/host volumes |
| `ferrofin.env` / `ferrofin.envFrom` / `ferrofin.args` | `[]` | Config via `FERROFIN_*` env or CLI flags |
| `livenessProbe` / `readinessProbe` | `/health/live` / `/health/ready` | Ferrofin's health endpoints |
| `strategy` | RollingUpdate, surge 1 / unavailable 0 | Upgrade without dropping playback; set `type: Recreate` if the config PVC can't be mounted twice |
| `ingress.enabled` | `false` | Standard `Ingress` (most clusters expose Ferrofin this way) |
| `httpRoute.enabled` | `false` | Gateway API `HTTPRoute` (alternative to ingress) |
| `networkPolicy.enabled` | `false` | Pod isolation (needs a policy-enforcing CNI) |
| `serviceAccount.create` | `true` | Dedicated service account |
| `metrics.enabled` / `metrics.serviceMonitor.enabled` | `false` | Scrape Ferrofin's Prometheus `/metrics` endpoint via a `ServiceMonitor` |

To expose Ferrofin on most clusters, enable ingress:

```yaml
ingress:
  enabled: true
  className: nginx
  hosts:
    - host: ferrofin.example.com
      paths: [{ path: /, pathType: Prefix }]
  tls:
    - secretName: ferrofin-tls
      hosts: [ferrofin.example.com]
```

Health probes target Ferrofin's real endpoints (`GET /health/live`, `GET /health/ready`);
there is no Jellyfin-style `/health`.

## LAN server discovery

Ferrofin answers Jellyfin discovery requests on UDP 7359. `discovery.enabled`
controls chart exposure only; the persisted network setting `AutoDiscovery` controls
whether the application listens and takes effect after restart. The chart creates a
separate UDP Service so HTTP LoadBalancers need not support mixed protocols.
ClusterIP, NodePort, LoadBalancer, ingress and HTTPRoute do not themselves carry LAN
broadcasts to a pod.

For a node attached to the clients' LAN, opt into host networking:

```yaml
hostNetwork: true
strategy:
  type: Recreate
discovery:
  enabled: true
ferrofin:
  config:
    FERROFIN_PUBLISHED_URL: "http://192.168.1.10:8096"
```

Replace the example address with the selected node's reachable LAN address, and use
`nodeSelector` or affinity to keep the pod on that node. The chart sets
`dnsPolicy: ClusterFirstWithHostNet`. Keep one replica and leave TCP 8096 and UDP
7359 free on the node; `Recreate` prevents an upgrade's two pods from competing for
those ports. Node firewall rules must permit the traffic. With host networking,
NetworkPolicy enforcement depends on the CNI; apply host firewall policy as needed.
For ordinary pod networking, the discovery NetworkPolicy rule follows the same
source selectors as HTTP; restrictive selectors may exclude external LAN clients.

A LAN-attached pod network is another option if the CNI delivers broadcasts and
clients can route to the advertised HTTP address. See [deployment and verification
instructions](../../docs/SERVER_DISCOVERY.md). Test from a separate LAN client;
unicast access to a Service IP alone does not establish broadcast discovery.
