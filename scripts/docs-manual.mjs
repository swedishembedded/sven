// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for documentation that stays
// consistent across web, PDF and repository for its clients. If your team
// needs expertise in documentation tooling then you can procure our services
// by sending an email to info@swedishembedded.com.
//
// Concatenates the pages of a documentation publication (build/docs) into one
// Markdown manual, in the order of the publication's navigation - which is the
// order docs/manifest.yaml declares. Nothing else decides the order.
//
// Each page becomes a chapter whose first heading carries the anchor
// `page-<route>`. Links between chapters are rewritten to those anchors; a
// link to a page that is not part of the manual points at the published site.
//
// Usage: node scripts/docs-manual.mjs --dir build/docs --out FILE
//            [--sections "Getting started,Guides"]
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, posix } from "node:path";

const SITE = "https://swedishembedded.com/docs";

function parseArgs(argv) {
  const args = { sections: null };
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i]?.replace(/^--/, "");
    if (!["dir", "out", "sections"].includes(key) || argv[i + 1] === undefined) {
      throw new Error(`usage: docs-manual.mjs --dir DIR --out FILE [--sections "A,B"]`);
    }
    args[key] = argv[i + 1];
  }
  if (!args.dir || !args.out) throw new Error("--dir and --out are required");
  if (args.sections) args.sections = args.sections.split(",").map((s) => s.trim());
  return args;
}

/** Routes of a navigation subtree, in order. */
function routesOf(nodes) {
  return nodes.flatMap((n) => (n.route !== undefined ? [n.route] : []).concat(routesOf(n.children ?? [])));
}

const anchorOf = (route) => `page-${route === "" ? "home" : route.replaceAll("/", "-")}`;

/** Applies `fn` to every line outside fenced code blocks. */
function mapOutsideFences(text, fn) {
  let fence = null;
  return text
    .split("\n")
    .map((line) => {
      const m = /^ {0,3}(`{3,}|~{3,})/.exec(line);
      if (m) {
        if (!fence) fence = m[1][0];
        else if (m[1][0] === fence) fence = null;
        return line;
      }
      return fence ? line : fn(line);
    })
    .join("\n");
}

function chapter(page, pageByFile, inManual, pub) {
  const body = readFileSync(`${pub.dir}/${page.file}`, "utf8");
  const here = posix.dirname(page.file);
  let anchored = false;
  const rewritten = mapOutsideFences(body, (line) => {
    if (!anchored && /^# /.test(line)) {
      anchored = true;
      line = `${line} {#${anchorOf(page.route)}}`;
    }
    return line.replace(/\]\(([^)\s]+)\)/g, (whole, url) => {
      if (/^[a-z][a-z0-9+.-]*:/i.test(url) || url.startsWith("#")) return whole;
      const [path, fragment] = url.split("#");
      const target = posix.normalize(posix.join(here, path));
      if (target.startsWith("assets/")) return `](${target})`;
      const linked = pageByFile.get(target);
      if (!linked) return whole;
      if (inManual.has(linked.route)) return `](#${anchorOf(linked.route)})`;
      return `](${SITE}/${pub.project}/${linked.route}${fragment ? `#${fragment}` : ""})`;
    });
  });
  if (!anchored) throw new Error(`${page.file}: no top-level heading to anchor`);
  return rewritten.replace(/\n*$/, "\n");
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const manifest = JSON.parse(readFileSync(`${args.dir}/manifest.json`, "utf8"));
  const groups = manifest.navigation.filter((g) => !args.sections || args.sections.includes(g.title));
  if (args.sections) {
    const missing = args.sections.filter((s) => !manifest.navigation.some((g) => g.title === s));
    if (missing.length) throw new Error(`no such navigation section: ${missing.join(", ")}`);
  }
  const routes = [""].concat(routesOf(groups).filter((r) => r !== ""));
  const pageByRoute = new Map(manifest.pages.map((p) => [p.route, p]));
  const pageByFile = new Map(manifest.pages.map((p) => [p.file, p]));
  const inManual = new Set(routes);
  const pub = { dir: args.dir, project: manifest.project.id };

  const chapters = routes.map((r) => {
    const page = pageByRoute.get(r);
    if (!page) throw new Error(`navigation names route "${r}" but the publication has no such page`);
    return chapter(page, pageByFile, inManual, pub);
  });
  mkdirSync(dirname(args.out), { recursive: true });
  writeFileSync(args.out, chapters.join("\n"));
  console.log(`manual: ${chapters.length} chapters -> ${args.out}`);
}

try {
  main();
} catch (e) {
  console.error(`docs-manual: ${e.message}`);
  process.exit(1);
}
