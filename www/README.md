# dcode-ai website

Marketing + install site for [dcode-ai](https://github.com/Dhanuzh/dcode-ai).
Vite + React + TypeScript + Tailwind v4, built to static files and served from
Cloudflare Pages.

## Local development

```bash
cd www
npm install
npm run dev      # http://localhost:5173
npm run build    # static output in dist/
npm run preview  # serve the production build
```

## Deploying to Cloudflare Pages

**Via the dashboard (recommended — auto-deploys on push):**

1. Cloudflare dashboard → **Workers & Pages** → **Create** → **Pages** →
   **Connect to Git**, and pick this repository.
2. Set the build configuration:

   | Setting | Value |
   | --- | --- |
   | Framework preset | `Vite` |
   | Build command | `npm run build` |
   | Build output directory | `dist` |
   | Root directory | `www` |

3. Deploy. Every push to `main` publishes; pull requests get preview URLs.

**Via Wrangler (manual, no Git integration):**

```bash
npm run build
npx wrangler pages deploy dist --project-name=dcode-ai
```

Add a custom domain under **Pages → your project → Custom domains**.

## Structure

```
src/
  App.tsx                 page sections (nav, hero, features, providers, install)
  data.ts                 all site copy + install commands — edit here first
  index.css               design tokens; mirrors the dcode-ai TUI theme
  components/
    Terminal.tsx          animated session replay in the hero
    CopyCommand.tsx       command line with copy-to-clipboard
public/
  _headers                Cloudflare security + caching headers
```

Content lives in `src/data.ts` — features, providers and install commands can be
updated without touching the layout.

## Notes

- Colors intentionally match the TUI's default theme (accent `#48d1cc`) so the
  site and the terminal read as one product.
- No images are shipped; the hero is rendered text, which keeps the page fast.
- Animations respect `prefers-reduced-motion`.
