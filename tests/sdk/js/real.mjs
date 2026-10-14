import assert from "node:assert/strict";
import { webcrypto } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
    importGeneratedEsm,
    locateGeneratedArtifacts,
    startTemporarySymbol,
} from "../mock-server.mjs";

if (globalThis.crypto === undefined) {
    Object.defineProperty(globalThis, "crypto", { value: webcrypto });
}

const root = resolve(process.cwd());
const artifacts = await locateGeneratedArtifacts(root);
const sdk = await importGeneratedEsm(artifacts.apiJs);
const dataRoot = await mkdtemp(join(tmpdir(), "symbol-sdk-js-real-"));
const binary = resolve(root, process.env.SYMBOL_BIN ?? "target/debug/symbol");
const server = await startTemporarySymbol({ binary, dataRoot, root });
const origin = server.origin;

try {
    const symbol = new sdk.SymbolClient({ origin });
    const initial = await symbol.stats();
    assert.equal(initial.sites, 0);

    const site = symbol.site("sdk-real");
    const created = await site.put("<h1>SDK</h1>", {
        mediaType: sdk.MediaTypes.Html,
    });
    assert.equal(created.status, 201);
    assert.equal(created.changed, true);
    assert.equal(created.undo.token.length > 0, true);

    const inventory = await site.files();
    assert.equal(inventory.status, 200);
    assert.equal(inventory.site, "sdk-real");
    assert.equal(inventory.files.some((file) => file.path === "index.html"), true);
    assert.equal(await site.file("index.html").text(), "<h1>SDK</h1>");

    const raw = await site.file("index.html").raw();
    assert.equal(raw.status, 200);
    assert.match(raw.headers.get("content-type"), /^text\/html(?:;|$)/);
    assert.equal(raw.headers.get("etag"), `"${await site.file("index.html").hash()}"`);
    assert.equal(await raw.text(), "<h1>SDK</h1>");
    await raw[Symbol.asyncDispose]();
    await assert.rejects(Promise.resolve(site.file("index").raw()), sdk.NotFoundError);

    const allocated = await site.folder("generated").text("allocated text", {
        name: { prefix: "note-", suffix: "-final", extension: ".TXT" },
    });
    assert.equal(allocated.status, 201);
    assert.match(allocated.path, /^generated\/note-[0-9a-f]{64}-final\.txt$/);
    assert.equal(allocated.naming.mode, "generated");
    assert.equal(allocated.blobUrl.pathname.startsWith("/.blob/sdk-real/"), true);
    await assert.rejects(Promise.resolve(site.file("generated").raw()), sdk.NotFoundError);

    const alias = await site.alias("latest", allocated.path);
    assert.equal(alias.target, allocated.path);
    assert.equal(await site.file("latest").text(), "allocated text");
    assert.equal(await site.file("latest").hash(), allocated.hash.replace(/^blake3:/, ""));

    const replaced = await site.file(allocated.path).replace("replacement text", {
        baseHash: allocated.hash,
    });
    assert.equal(replaced.outcome, "relocated");
    assert.equal(replaced.relocated, true);
    assert.notEqual(replaced.newPath, replaced.oldPath);

    const spliced = await site.file(replaced.newPath).splice({
        offset: 0,
        deleteBytes: 0,
        insert: new TextEncoder().encode("prefix "),
    }, {
        baseHash: replaced.newHash,
    });
    assert.equal(spliced.changed, true);
    assert.equal(
        await site.file(spliced.newPath).text(),
        "prefix replacement text",
    );

    const expiry = await site.file(spliced.newPath).setExpiry({
        mode: "relative",
        duration: "1h",
    });
    assert.equal(expiry.report.target.kind, "file");
    assert(expiry.report.effectiveExpiresAt instanceof Date);

    const generated = await site.files("generated");
    assert.equal(generated.status, 200);
    assert.equal(generated.entries.length >= 1, true);

    await site.file("notes/Topos Theory.md").put("# topos");
    await site.file("notes/sheaf.txt").put("sheaf");
    await symbol.site("sdk-other").file("topos.txt").put("t");
    const found = await symbol.search("topos");
    assert.equal(found.total, 2);
    assert.equal(found.site, null);
    assert.equal(found.nextPage, null);
    assert.deepEqual(
        found.results.map((hit) => `${hit.site}/${hit.path}`),
        ["sdk-other/topos.txt", "sdk-real/notes/Topos Theory.md"],
    );
    assert.equal(found.results[1].url.href, `${origin}/sdk-real/notes/Topos%20Theory.md`);
    assert(found.results[1].modified instanceof Date);
    const scoped = await site.search("TOPOS theory", { path: "notes" });
    assert.equal(scoped.path, "notes/");
    assert.deepEqual(scoped.results.map((hit) => hit.name), ["Topos Theory.md"]);
    const folder = (await symbol.search("notes")).results[0];
    assert.equal(folder.kind, "directory");
    assert.equal(folder.files, 2);
    assert.equal(folder.url.pathname, "/sdk-real/notes/");
    assert.deepEqual((await site.search("*.md")).results.map((hit) => hit.path), [
        "notes/Topos Theory.md",
    ]);
    assert.equal((await site.search("notes/")).total, 3);
    const firstPage = await symbol.search("topos", { limit: 1 });
    assert.equal(firstPage.nextPage, 2);
    const everything = [];
    for await (const hit of symbol.searchAll("topos", { limit: 1 })) {
        everything.push(hit.path);
    }
    assert.deepEqual(everything, found.results.map((hit) => hit.path));
    assert.equal((await symbol.search("topos", { page: 5 })).results.length, 0);
    await assert.rejects(
        Promise.resolve(symbol.search("topos", { site: "missing-site" })),
        sdk.NotFoundError,
    );
    await assert.rejects(
        Promise.resolve(symbol.search(Array(17).fill("a").join(" "))),
        sdk.ValidationError,
    );

    const archive = await site.archive("tar.gz");
    assert.equal((await archive.blob()).size > 0, true);
    await archive[Symbol.asyncDispose]();

    const popped = await site.remove();
    assert.equal(popped.undo.token.length > 0, true);
    await popped[Symbol.asyncDispose]();
    const restored = await site.undo(popped.undo.token);
    assert(restored.restoredAt instanceof Date);
    const restoredInventory = await site.files();
    assert.equal(restoredInventory.status, 200);
    assert.equal(restoredInventory.site, "sdk-real");

    assert.deepEqual(symbol.assertExactApi(), {
        apiVersion: sdk.API_VERSION_PARTS,
        absoluteRevision: sdk.API_REVISION,
        sourceHash: sdk.SOURCE_HASH,
    });
    console.log("SDK JavaScript real temporary workflow: ok");
} catch (error) {
    throw new Error(`${error instanceof Error ? error.stack : String(error)}\n${server.log}`);
} finally {
    await server.stop();
    await rm(dataRoot, { recursive: true, force: true });
}
