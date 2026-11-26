# Diagram style

Paths in this file are relative to the `docs` directory of the mirrord repository.

All diagrams in the docs use one style. Write each diagram as a hand-written SVG file in `docs/.gitbook/assets/`. Do not export diagrams from Excalidraw, D2, draw.io, or Figma, and do not add PNG diagrams. Screenshots of the product are not diagrams, and this guide does not apply to them.

Two diagrams show the style well. Copy from them when you start a new one:

- `docs/.gitbook/assets/api-gateway-traffic.svg`
- `docs/.gitbook/assets/header-propagation.svg`

## File

- Root element: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 W H" width="W" height="H" font-family="Inter, Helvetica, Arial, sans-serif" role="img" aria-labelledby="t d">`.
- Width: 960 to 1200. GitBook scales the image down to the page width, so a wider image makes the text too small. Use the same width in the `viewBox` and the `width` attribute. When the content needs more than 1200, keep the wider `viewBox` and set `width="1200"`, as in `redis-pubsub-splitting.svg`.
- Put a `<title id="t">` and a `<desc id="d">` first. The `desc` tells the full flow in words, for screen readers.
- Put a white background as the first shape: `<rect x="0" y="0" width="W" height="H" fill="#ffffff"/>`. GitBook dark mode needs it.
- Keep the file small and readable. Use plain `rect`, `line`, `path`, `polyline`, `circle`, `ellipse`, and `text`. Do not embed fonts, raster images, or base64 data. Add a short comment before each group of shapes.

## Colors

| Use | Fill | Text |
|-----|------|------|
| Neutral component (client, service, queue, database) | `#ffffff` | `#232141` |
| mirrord component (agent, operator, layer, CLI, forwarder) | `#756DF3` | `#ffffff` |
| Your local process or your machine | `#756DF3` | `#ffffff` |
| The deployed workload or the original target | `#E4E3FD` | `#232141` |
| Gateway, broker, or other infrastructure to highlight | `#FFCB7D` | `#232141` |
| Temporary resource that mirrord creates | `#ffffff`, dashed `#756DF3` stroke, `stroke-dasharray="8 6"` | `#232141` |
| Boundary (cluster, machine, namespace) | `#F5F4FE` (`#ffffff` for a boundary inside a boundary), solid `#756DF3` stroke | label `#4a44b8` |
| Inactive or dropped path | none | `#8a8a99` |

- Outline every box with `stroke="#232141" stroke-width="2"`, except boundaries and temporary resources, which use the purple stroke above.
- Only temporary resources use a dashed outline. A boundary has a solid outline, so the two never look the same.
- Secondary text inside boxes uses `#333344` (or `#ffffff` on a purple box).
- Do not use other colors. Do not use red, green, or blue to show "match" and "no match". Use the purple path for the mirrord route and the dark path for the normal route.

## Shapes

- Box: `rx="12"`. A box inside a box: `rx="8"` to `rx="10"`.
- Database: a cylinder (an `ellipse` top on a `path` body), same stroke and colors as a box.
- Queue or topic: a box with three thin vertical lines at the right end, or a plain box with the type in the secondary text.
- Do not draw hand-drawn or sketchy shapes. Use the symbols in [Symbols](#symbols) to show what a thing is.

## Logos

### Kubernetes logo

The Kubernetes logo shows what runs in the cluster. Use it in two places:

- **On a Kubernetes cluster.** On a cluster drawn as a cloud, center the logo on the top-right corner bump, 24px in from the top of the bump, as in `api-gateway-traffic.svg`. The boundary rule that follows does not apply to a cloud. On a cluster drawn as a boundary, center a 36px logo on the top-right corner, at `x + width - 18`, `y - 18`. Do not put it on a broker boundary, such as "Kafka cluster". A broker boundary gets the broker logo (see below).
- **On a box that runs in the cluster, when the diagram has no Kubernetes cluster boundary.** Center a 36px logo on the bottom-right corner of the box, at `x + width - 18`, `y + height - 18`. On a box less than 60 high, use a 26px logo, at `x + width - 13`, `y + height - 13`. Examples: the mirrord operator, a forwarder, the deployed app, a preview pod. Do not put it on brokers or managed services.

Define the symbol once in `<defs>`, and place it with `<use href="#k8s" x="..." y="..." width="36" height="36"/>`. The logo half covers the corner, so make sure it does not cover an arrow or text, and grow the canvas when it goes past the edge:

```xml
<symbol id="k8s" viewBox="0 0 32 32">
  <polygon points="16.00,1.60 27.73,7.25 30.62,19.94 22.51,30.11 9.49,30.11 1.38,19.94 4.27,7.25" fill="#326CE5" stroke="#326CE5" stroke-width="2" stroke-linejoin="round"/>
  <g fill="none" stroke="#ffffff" stroke-width="2" stroke-linecap="round">
  <circle cx="16" cy="16" r="7"/>
  <line x1="16.00" y1="13.40" x2="16.00" y2="5.80"/><line x1="18.03" y1="14.38" x2="23.97" y2="9.64"/><line x1="18.53" y1="16.58" x2="25.94" y2="18.27"/><line x1="17.13" y1="18.34" x2="20.43" y2="25.19"/><line x1="14.87" y1="18.34" x2="11.57" y2="25.19"/><line x1="13.47" y1="16.58" x2="6.06" y2="18.27"/><line x1="13.97" y1="14.38" x2="8.03" y2="9.64"/>
  </g>
  <circle cx="16" cy="16" r="2.4" fill="#ffffff"/>
</symbol>
```

### Broker logos

A boundary for a message broker cluster, such as "Kafka cluster" or "RabbitMQ cluster", gets the broker logo. Center a 36px logo on the top-right corner of the boundary, the same as the Kubernetes logo. The glyphs come from [simple-icons](https://simpleicons.org). Define the symbol in `<defs>` and place it with `<use href="#kafka" .../>` or `<use href="#rabbitmq" .../>`:

```xml
<symbol id="kafka" viewBox="0 0 32 32">
      <circle cx="16" cy="16" r="15" fill="#ffffff" stroke="#232141" stroke-width="1.5"/>
      <path transform="translate(5.5 5.5) scale(0.875)" fill="#231F20" d="M9.71 2.136a1.43 1.43 0 0 0-2.047 0h-.007a1.48 1.48 0 0 0-.421 1.042c0 .41.161.777.422 1.039l.007.007c.257.264.616.426 1.019.426.404 0 .766-.162 1.027-.426l.003-.007c.261-.262.421-.629.421-1.039 0-.408-.159-.777-.421-1.042H9.71zM8.683 22.295c.404 0 .766-.167 1.027-.429l.003-.008c.261-.261.421-.631.421-1.036 0-.41-.159-.778-.421-1.044H9.71a1.42 1.42 0 0 0-1.027-.432 1.4 1.4 0 0 0-1.02.432h-.007c-.26.266-.422.634-.422 1.044 0 .406.161.775.422 1.036l.007.008c.258.262.617.429 1.02.429zm7.89-4.462c.359-.096.683-.33.882-.684l.027-.052a1.47 1.47 0 0 0 .114-1.067 1.454 1.454 0 0 0-.675-.896l-.021-.014a1.425 1.425 0 0 0-1.078-.132c-.36.091-.684.335-.881.686-.2.349-.241.75-.146 1.119.099.363.33.691.675.896h.002c.346.203.737.239 1.101.144zm-6.405-7.342a2.083 2.083 0 0 0-1.485-.627c-.58 0-1.103.242-1.482.627-.378.385-.612.916-.612 1.507s.233 1.124.612 1.514a2.08 2.08 0 0 0 2.967 0c.379-.39.612-.923.612-1.514s-.233-1.122-.612-1.507zm-.835-2.51c.843.141 1.6.552 2.178 1.144h.004c.092.093.182.196.265.299l1.446-.851a3.176 3.176 0 0 1-.047-1.808 3.149 3.149 0 0 1 1.456-1.926l.025-.016a3.062 3.062 0 0 1 2.345-.306c.77.21 1.465.721 1.898 1.482v.002c.431.757.518 1.626.313 2.408a3.145 3.145 0 0 1-1.456 1.928l-.198.118h-.02a3.095 3.095 0 0 1-2.154.201 3.127 3.127 0 0 1-1.514-.944l-1.444.848a4.162 4.162 0 0 1 0 2.879l1.444.846c.413-.47.939-.789 1.514-.944a3.041 3.041 0 0 1 2.371.319l.048.023v.002a3.17 3.17 0 0 1 1.408 1.906 3.215 3.215 0 0 1-.313 2.405l-.026.053-.003-.005a3.147 3.147 0 0 1-1.867 1.436 3.096 3.096 0 0 1-2.371-.318v-.006a3.156 3.156 0 0 1-1.456-1.927 3.175 3.175 0 0 1 .047-1.805l-1.446-.848a3.905 3.905 0 0 1-.265.294l-.004.005a3.938 3.938 0 0 1-2.178 1.138v1.699a3.09 3.09 0 0 1 1.56.862l.002.004c.565.572.914 1.368.914 2.243 0 .873-.35 1.664-.914 2.239l-.002.009a3.1 3.1 0 0 1-2.21.931 3.1 3.1 0 0 1-2.206-.93h-.002v-.009a3.186 3.186 0 0 1-.916-2.239c0-.875.35-1.672.916-2.243v-.004h.002a3.1 3.1 0 0 1 1.558-.862v-1.699a3.926 3.926 0 0 1-2.176-1.138l-.006-.005a4.098 4.098 0 0 1-1.173-2.874c0-1.122.452-2.136 1.173-2.872h.006a3.947 3.947 0 0 1 2.176-1.144V6.289a3.137 3.137 0 0 1-1.558-.864h-.002v-.004a3.192 3.192 0 0 1-.916-2.243c0-.871.35-1.669.916-2.243l.002-.002A3.084 3.084 0 0 1 8.683 0c.861 0 1.641.355 2.21.932v.002h.002c.565.574.914 1.372.914 2.243 0 .876-.35 1.667-.914 2.243l-.002.005a3.142 3.142 0 0 1-1.56.864v1.692zm8.121-1.129l-.012-.019a1.452 1.452 0 0 0-.87-.668 1.43 1.43 0 0 0-1.103.146h.002c-.347.2-.58.529-.677.896-.095.365-.054.768.146 1.119l.007.009c.2.347.519.579.874.673.357.103.755.059 1.098-.144l.019-.009a1.47 1.47 0 0 0 .657-.885 1.493 1.493 0 0 0-.141-1.118"/>
    </symbol>
<symbol id="rabbitmq" viewBox="0 0 32 32">
      <circle cx="16" cy="16" r="15" fill="#ffffff" stroke="#232141" stroke-width="1.5"/>
      <path transform="translate(5.5 5.5) scale(0.875)" fill="#FF6600" d="M23.035 9.601h-7.677a.956.956 0 01-.962-.962V.962a.956.956 0 00-.962-.956H10.56a.956.956 0 00-.962.956V8.64a.956.956 0 01-.962.962H5.762a.956.956 0 01-.961-.962V.962A.956.956 0 003.839 0H.959a.956.956 0 00-.956.962v22.076A.956.956 0 00.965 24h22.07a.956.956 0 00.962-.962V10.58a.956.956 0 00-.962-.98zm-3.86 8.152a1.437 1.437 0 01-1.437 1.443h-1.924a1.437 1.437 0 01-1.436-1.443v-1.917a1.437 1.437 0 011.436-1.443h1.924a1.437 1.437 0 011.437 1.443z"/>
    </symbol>
```

## Symbols

Use a symbol, not only a label, when a symbol shows what a thing is. A reader recognizes a laptop or a database faster than they read a box title.

### mirrord logo

Put the mirrord logo (the mirror) on every box that is a part of mirrord: the operator, an agent, `mirrord-layer`, the mirrord CLI, a forwarder in the operator, and a local process that runs with mirrord. Place it inside the box, to the left of the title, and move the title to the right to make space. The logo is 32 wide and 57 high. GitBook scales a diagram down to the page width, and a smaller logo looks grainy on a purple box. When the box is too small, use the largest size that keeps 6px of space above and below the logo, and keep the width at 0.56 of the height. For a new diagram, make the box tall enough for a logo of at least 22 by 40:

```xml
<use href="#mirrord" x="..." y="..." width="32" height="57"/>
```

The symbol is the mirror from `images/logo.svg` at the repository root, without the wordmark, the thin white outline, and the glare lines on the glass. Those details look grainy at icon size. Copy the `<symbol id="mirrord" viewBox="97 61 391 696">` definition from `docs/.gitbook/assets/api-gateway-traffic.svg`. Do not change it, and do not use the full logo in a diagram.

### Laptop

Draw a developer machine or a local process as a laptop: a dark screen frame and a keyboard deck under it. Do not draw a CI runner as a laptop (see [Server](#server)). The example is a laptop whose screen frame is the rect `x=100, y=100, width=200, height=120`. Arrows end at the edge of the frame.

```xml
<rect x="100" y="100" width="200" height="120" rx="10" fill="#232141"/>
<rect x="108" y="108" width="184" height="104" rx="4" fill="#756DF3"/>
<path d="M100 220 H300 L316 234 H84 Z" fill="#ffffff" stroke="#232141" stroke-width="2" stroke-linejoin="round"/>
<line x1="110" y1="225" x2="290" y2="225" stroke="#232141" stroke-width="3" stroke-dasharray="6 3"/>
<rect x="184" y="228.5" width="32" height="3.5" rx="1.5" fill="#232141"/>
<rect x="80" y="234" width="240" height="5" rx="2.5" fill="#232141"/>
```

For a frame `X, Y, W, H`: the inner rect is inset 8; the deck goes from `(X, Y+H)` to `(X+W+16, Y+H+14)` and back to `(X-16, Y+H+14)`; the key line is at `Y+H+5`, from `X+10` to `X+W-10`; the trackpad is 32 wide at `X+W/2-16, Y+H+8.5`; and the lip is `X-20, Y+H+14, W+40, 5`. The laptop reaches 20 beyond the frame on each side and 19 below it. Put the content 8 or more inside the frame. The inner rect has the fill of the role: `#756DF3` when the screen is a local process, or the boundary fill `#F5F4FE` when the screen holds other boxes, such as `mirrord-layer`, a terminal, or `mirrord.json`. When the screen holds one box, such as "Local process", you can also put the box on a white screen, as in `operator-architecture.svg`.

### Server

Draw a CI runner or another machine that is not a developer laptop as a server: the dark frame of a laptop without the keyboard deck, with a status strip along the bottom of the frame. The strip has two status lights on the left and vent lines on the right. Copy it from `mirrord-ci-diagram.svg`.

### Cluster

Draw a Kubernetes cluster as a cloud when the diagram shows one cluster. A cloud has bumps on all four sides. Do not draw straight sides with bumps only on the top, because that shape looks like a loaf of bread. The cloud holds the whole rect `x, y, w, h` that you plan for its content. The top bumps reach about 0.12 of `w` above the rect, the side bumps about 16px beside it, and the bottom bumps about 0.05 of `w` below it. Keep that space free of laptops, labels, and other boxes. Copy the path from `shared-cluster.svg` and scale it, or calculate it:

- `a = min(0.30 h, 0.20 w)`, `cb = min(0.16 h, 0.09 w)`.
- Go clockwise from `(x, y+h-cb)`. Join the points with clockwise arcs (`A r r 0 0 1`).
- Left side: points about every 90px up to `(x, y+a)`. Each arc bulges `min(0.18 chord, 16)` px.
- Top-left corner: `(x+0.16w, y)`, a half circle. Top: `(x+0.43w, y)`, `(x+0.66w, y)`, `(x+0.88w, y)`, with a radius of 0.56 of the chord.
- Top-right corner: `(x+w, y+0.8a)`, a half circle. Right side: points about every 90px down to `(x+w, y+h-cb)`, as on the left.
- Bottom-right corner: `(x+w-cb, y+h)`, a half circle. Bottom: `(x+0.62w, y+h)`, `(x+0.30w, y+h)`, `(x+cb, y+h)`, with a radius of 1.1 of the chord. Bottom-left corner: back to the start, a half circle.

Use `fill="#F5F4FE" stroke="#756DF3" stroke-width="2"`. Put the cluster label inside the rect, clear of the arcs. When the diagram shows several clusters side by side, such as the multi-cluster diagrams, draw each cluster as a boundary: there is no space for the bumps.

### Stack

Draw more than one of a thing as a stack: two copies of the box behind it, each one 8 up and 8 to the right. Use it for "other services", replicas, workers, consumers, and publishers. Draw the back copies first:

```xml
<rect x="X+16" y="Y-16" width="W" height="H" rx="10" fill="#ffffff" stroke="#232141" stroke-width="2"/>
<rect x="X+8" y="Y-8" width="W" height="H" rx="10" fill="#ffffff" stroke="#232141" stroke-width="2"/>
<rect x="X" y="Y" width="W" height="H" rx="10" fill="#ffffff" stroke="#232141" stroke-width="2"/>
```

Do not use a stack when the diagram shows the copies one by one, for example three pods with one agent each.

### Database

Draw a database as a cylinder, with the role fill. A database branch is a temporary resource, so its cylinder is white with a dashed `#756DF3` outline.

### Message

Put an envelope on the arrow from a producer or publisher to a queue, topic, or subscription, to show that messages move there. Center it on the arrow, 28 wide and 20 high, and keep the arrow label next to it:

```xml
<symbol id="envelope" viewBox="0 0 28 20">
  <rect x="1" y="1" width="26" height="18" rx="3" fill="#ffffff" stroke="#232141" stroke-width="2"/>
  <path d="M2 3 L14 11.5 L26 3" fill="none" stroke="#232141" stroke-width="2" stroke-linejoin="round"/>
</symbol>
```

### Cloud icon

The ECS diagram uses a small cloud (`#cloud`) for a remote environment. The env vars diagram uses it for the remote environment that the agent sends back. Copy it from `docs/.gitbook/assets/ecs-sample-app.svg` when another diagram needs the same meaning.

## Text

- Box title: `font-size="16" font-weight="700"`. Smaller boxes may use 14.
- Secondary text: `font-size="12.5"`.
- Boundary label: `font-size="15" font-weight="700" fill="#4a44b8"`, top-left inside the boundary.
- Arrow label: `font-size="12.5"`, `#4a44b8` for the mirrord route, `#333344` for others.
- Code, headers, queue names, and resource names: `font-family="Menlo, Consolas, monospace"`.
- Do not put a title in the image. The page caption (the alt text) is the title.
- Use sentence case. Text must not touch a box edge or another text line. Keep at least 10px of space.

## Arrows

- Normal route: `stroke="#232141" stroke-width="2.5"` with `marker-end="url(#arrow)"`.
- mirrord route (traffic that mirrord sends to you): `stroke="#756DF3"` with `marker-end="url(#arrow-purple)"`.
- Dropped or inactive route: `stroke="#ACACAC" stroke-dasharray="6 5"` with `marker-end="url(#arrow-grey)"`.
- Define the markers once in `<defs>`:

```xml
<marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" orient="auto-start-reverse">
  <path d="M0 0 L10 5 L0 10 z" fill="#232141"/>
</marker>
<marker id="arrow-purple" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" orient="auto-start-reverse">
  <path d="M0 0 L10 5 L0 10 z" fill="#756DF3"/>
</marker>
<marker id="arrow-grey" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" orient="auto-start-reverse">
  <path d="M0 0 L10 5 L0 10 z" fill="#ACACAC"/>
</marker>
```

- Use straight lines or right-angle `polyline`s. Do not use curves, except for a loop. Arrows must not cross boxes or labels.
- Flow goes left to right, or top to bottom.

## Check a diagram

Render the file and look at it before you open a PR. Run this command from the `docs` directory:

```sh
rsvg-convert -w 1200 -b white docs/.gitbook/assets/<name>.svg -o /tmp/<name>.png
```
