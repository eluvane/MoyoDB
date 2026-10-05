import fs from 'node:fs';
import path from 'node:path';

const directory = path.resolve(process.env.OUTPUT_DIR ?? path.join(process.cwd(), 'bench-results/crossover'));
const modes = process.argv.slice(2).length ? process.argv.slice(2) : ['persistent', 'incognito'];
const median = (values) => {
    const sorted = [...values].sort((a, b) => a - b);
    const middle = Math.floor(sorted.length / 2);
    return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
const comparisons = [];
for (const mode of modes) {
    const before = JSON.parse(fs.readFileSync(`${directory}/crossover-five-${mode}-baseline.json`));
    const after = JSON.parse(fs.readFileSync(`${directory}/crossover-five-${mode}-combined.json`));
    if (before.execution.userDataDir !== after.execution.userDataDir) throw Error('profile differs between artifacts');
    if (
        [before, after].some(
            (report) =>
                report.execution.designVersion !== 2 ||
                report.execution.pageLifetime !== 'fresh-per-artifact-suite' ||
                !report.execution.fetchVerification.allRoundsVerified ||
                !report.execution.fetchVerification.allAssetHashesMatch ||
                !report.execution.fetchVerification.allMainPageProbesVerified ||
                report.execution.pairedRounds !== 6
        )
    ) {
        throw Error('fresh-page lifecycle, artifact fetch, or round verification missing');
    }
    const pageCreationIndices = [before, after].flatMap((report) => report.execution.pageCreationIndices);
    if (pageCreationIndices.length !== 12 || new Set(pageCreationIndices).size !== 12) {
        throw Error('each artifact suite must use a distinct fresh page');
    }
    for (const report of [before, after]) {
        const verification = report.execution.fetchVerification;
        for (let round = 0; round < 6; round++) {
            const expectedWorkers = 5 * (round === 0 ? 2 : 1);
            const expectedLoads = expectedWorkers + 1;
            const probe = verification.mainPageProbes[round];
            if (
                verification.dbWorkerLoads[round] !== expectedWorkers ||
                verification.expectedLoads[round] !== expectedLoads ||
                verification.wasmLoads[round] !== expectedLoads ||
                verification.glueLoads[round] !== expectedLoads ||
                Object.values(verification.snippetLoadsByPath[round]).some((loads) => loads !== expectedLoads) ||
                probe.buildProfile !== 'release' ||
                probe.wasm.sha256 !== report.execution.wasmSha256 ||
                [probe.wasm, probe.glue, ...probe.snippets].some(
                    (response) => response.sha256 !== response.expectedSha256
                )
            ) {
                throw Error(`main-page/Worker engine loads were not symmetric in round ${round}`);
            }
        }
    }
    for (const b of before.results.filter((row) => row.engine === 'moyodb')) {
        const a = after.results.find((row) => row.engine === 'moyodb' && row.workloadName === b.workloadName);
        const ib = before.results.find((row) => row.engine === 'indexeddb' && row.workloadName === b.workloadName);
        const ia = after.results.find((row) => row.engine === 'indexeddb' && row.workloadName === b.workloadName);
        const rows = [b, a, ib, ia];
        if (rows.some((row) => row.status !== 'ok' || row.rawSamples.length !== 6 || row.warmupSamples.length !== 1)) {
            throw Error(`invalid crossover result ${mode}/${b.workloadName}`);
        }
        for (let sample = 0; sample < 6; sample++) {
            const checksums = rows.map((row) => row.contentChecksums[sample]);
            if (checksums.some((checksum) => checksum !== checksums[0])) {
                throw Error(`crossover parity failed ${mode}/${b.workloadName}/${sample}`);
            }
        }
        const paired = b.rawSamples.map((beforeMs, round) => ({
            round,
            order: round % 2 === 0 ? 'AB' : 'BA',
            beforeMs,
            afterMs: a.rawSamples[round],
            deltaMs: a.rawSamples[round] - beforeMs,
            deltaPct: 100 * (a.rawSamples[round] / beforeMs - 1),
            indexedDbBeforeMs: ib.rawSamples[round],
            indexedDbAfterMs: ia.rawSamples[round]
        }));
        const moyoBefore = median(b.rawSamples);
        const moyoAfter = median(a.rawSamples);
        const indexedDbPooledMedian = median([...ib.rawSamples, ...ia.rawSamples]);
        comparisons.push({
            mode,
            workload: b.workloadName,
            recordCount: b.recordCount,
            valueSize: b.valueSize,
            batchSize: b.batchSize,
            moyoBefore,
            moyoAfter,
            indexedDbPooledMedian,
            gapBefore: moyoBefore / indexedDbPooledMedian,
            gapAfter: moyoAfter / indexedDbPooledMedian,
            latencyReductionPct: 100 * (1 - moyoAfter / moyoBefore),
            raw: {
                moyoBefore: b.rawSamples,
                moyoAfter: a.rawSamples,
                idbBefore: ib.rawSamples,
                idbAfter: ia.rawSamples
            },
            paired,
            pairedMedianDeltaMs: median(paired.map((row) => row.deltaMs)),
            pairedMedianDeltaPct: median(paired.map((row) => row.deltaPct)),
            byOrder: ['AB', 'BA'].map((order) => {
                const selected = paired.filter((row) => row.order === order);
                return {
                    order,
                    beforeMedian: median(selected.map((row) => row.beforeMs)),
                    afterMedian: median(selected.map((row) => row.afterMs)),
                    pairedMedianDeltaMs: median(selected.map((row) => row.deltaMs)),
                    pairedMedianDeltaPct: median(selected.map((row) => row.deltaPct)),
                    indexedDbBeforeMedian: median(selected.map((row) => row.indexedDbBeforeMs)),
                    indexedDbAfterMedian: median(selected.map((row) => row.indexedDbAfterMs))
                };
            }),
            idbControl: { beforeMedian: median(ib.rawSamples), afterMedian: median(ia.rawSamples) },
            checksums: b.contentChecksums,
            sha: { before: before.execution.wasmSha256, after: after.execution.wasmSha256 }
        });
    }
}
const output = {
    designVersion: 2,
    method: 'Predeclared comparison for these five workloads: one browser/profile/origin, with a fresh page for each artifact suite; six paired samples per artifact, balanced three AB and three BA rounds; one initial warmup. The previous page closes after completed stock cleanup and before artifact selection changes. Each fresh page runs the unchanged environment probe, keeping its selected WASM instance alive only for that suite, equally for both artifacts. Exact load counts verify one main-page probe plus every DB Worker; all served JS/WASM/snippet SHAs match the artifact manifest. Identical no-store headers apply to both artifacts. Moyo uses conventional median of all six samples; common strict IndexedDB baseline uses conventional pooled median of all twelve contemporaneous samples. All raw arrays and seed/content checksums retained. No selection, normalization or merging with earlier experiments. The earlier one-page campaign retained only the first artifact in its main-page module cache and is a separate confounded diagnostic. Empty-open remains separate because asset cache-control differs.',
    comparisons
};
fs.writeFileSync(`${directory}/crossover-five-comparison.json`, JSON.stringify(output, null, 2) + '\n');
for (const row of comparisons) {
    console.log(
        row.mode,
        row.workload,
        [row.moyoBefore, row.moyoAfter, row.indexedDbPooledMedian, row.gapBefore, row.gapAfter, row.latencyReductionPct]
            .map((value) => value.toFixed(2))
            .join(' ')
    );
    console.log('  byOrder', JSON.stringify(row.byOrder));
}
