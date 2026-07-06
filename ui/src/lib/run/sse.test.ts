import { describe, expect, it } from 'vitest';
import type { LogLine, RunDetail } from '$lib/api';
import {
	applyItems,
	applyLog,
	applyRun,
	applySnapshot,
	applyTask,
	initialState,
	MAX_LOG_LINES,
	mergeLogs,
	orderTasks,
	type SnapshotPayload
} from './sse';
import type { FlowDefinition } from '$lib/api';

const NOW = '2026-07-05T12:10:00Z';

function detail(): RunDetail {
	return {
		run: {
			id: 7,
			flow_id: 'health-fan',
			flow_rev: 1,
			status: 'running',
			trigger: 'manual',
			inputs: {},
			scheduled_for: null,
			started_at: '2026-07-05T12:00:00Z',
			finished_at: null,
			error: null,
			duration_sec: null,
			tasks_done: 1,
			tasks_total: 3
		},
		tasks: [
			{
				id: 1,
				run_id: 7,
				task_id: 'first',
				status: 'success',
				attempt: 1,
				result: null,
				outputs: { ids: [1, 2, 3] },
				error: null,
				started_at: '2026-07-05T12:00:00Z',
				finished_at: '2026-07-05T12:00:04Z'
			},
			{
				id: 2,
				run_id: 7,
				task_id: 'fan',
				status: 'running',
				attempt: 1,
				result: null,
				outputs: null,
				error: null,
				started_at: '2026-07-05T12:00:04Z',
				finished_at: null
			},
			{
				id: 3,
				run_id: 7,
				task_id: 'last',
				status: 'pending',
				attempt: 0,
				result: null,
				outputs: null,
				error: null,
				started_at: null,
				finished_at: null
			}
		],
		fanout: {
			fan: { total: 12, queued: 6, running: 3, success: 2, failed: 1, dropped: 0, retried: 0 }
		}
	};
}

function snapshot(lastLogId = 40): SnapshotPayload {
	return { ...detail(), last_log_id: lastLogId };
}

function log(id: number, message = `line ${id}`): LogLine {
	return { id, ts: NOW, level: 'INFO', task: 'flow', message };
}

describe('applySnapshot', () => {
	it('replaces detail, seeds itemsAgg from fanout and advances maxLogId', () => {
		const s = initialState();
		const fetchAfter = applySnapshot(s, snapshot(40));
		expect(s.detail?.run.id).toBe(7);
		expect(s.itemsAgg.fan).toEqual({
			total: 12,
			queued: 6,
			running: 3,
			success: 2,
			failed: 1,
			dropped: 0,
			retried: 0,
			throughput_per_sec: 0
		});
		expect(s.maxLogId).toBe(40);
		// history must be fetched from the beginning
		expect(fetchAfter).toBe(0);
		// last_log_id must not leak into detail
		expect('last_log_id' in (s.detail as object)).toBe(false);
	});

	it('re-snapshot (lagged resync) keeps throughput and asks for the gap only', () => {
		const s = initialState();
		applySnapshot(s, snapshot(40));
		applyItems(s, {
			task_id: 'fan',
			total: 12,
			queued: 4,
			running: 3,
			success: 4,
			failed: 1,
			dropped: 0,
			retried: 1,
			throughput_per_sec: 2.5
		});
		const fetchAfter = applySnapshot(s, snapshot(90));
		expect(fetchAfter).toBe(40);
		expect(s.maxLogId).toBe(90);
		expect(s.itemsAgg.fan.throughput_per_sec).toBe(2.5);
	});

	it('does not regress maxLogId when the snapshot is older than seen logs', () => {
		const s = initialState();
		applySnapshot(s, snapshot(40));
		applyLog(s, log(50));
		expect(applySnapshot(s, snapshot(45))).toBeNull();
		expect(s.maxLogId).toBe(50);
	});
});

describe('log dedup rule', () => {
	it('drops log events with id <= last_log_id from the snapshot', () => {
		const s = initialState();
		applySnapshot(s, snapshot(40));
		expect(applyLog(s, log(39))).toBe(false);
		expect(applyLog(s, log(40))).toBe(false);
		expect(s.logs).toHaveLength(0);
		expect(applyLog(s, log(41))).toBe(true);
		expect(s.logs.map((l) => l.id)).toEqual([41]);
		expect(s.maxLogId).toBe(41);
	});

	it('drops replays of already-appended live lines (<= max seen)', () => {
		const s = initialState();
		applySnapshot(s, snapshot(0));
		expect(applyLog(s, log(1))).toBe(true);
		expect(applyLog(s, log(2))).toBe(true);
		expect(applyLog(s, log(2))).toBe(false);
		expect(applyLog(s, log(1))).toBe(false);
		expect(s.logs.map((l) => l.id)).toEqual([1, 2]);
	});

	it('full sequence: snapshot -> dup log -> fresh log -> resync -> dup again', () => {
		const s = initialState();
		applySnapshot(s, snapshot(10));
		applyLog(s, log(10)); // dup from bridge overlap
		applyLog(s, log(11));
		applyLog(s, log(12));
		applySnapshot(s, snapshot(12)); // lagged resync; 12 already seen
		applyLog(s, log(12)); // replayed after resync
		applyLog(s, log(13));
		expect(s.logs.map((l) => l.id)).toEqual([11, 12, 13]);
	});
});

describe('log retention cap', () => {
	it('applyLog keeps only the newest MAX_LOG_LINES and flags truncation', () => {
		const s = initialState();
		for (let id = 1; id <= MAX_LOG_LINES + 3; id++) applyLog(s, log(id));
		expect(s.logs).toHaveLength(MAX_LOG_LINES);
		expect(s.logs[0].id).toBe(4); // 1..3 evicted
		expect(s.logs[s.logs.length - 1].id).toBe(MAX_LOG_LINES + 3);
		expect(s.logsTruncated).toBe(true);
	});

	it('stays untruncated at exactly the cap', () => {
		const s = initialState();
		for (let id = 1; id <= MAX_LOG_LINES; id++) applyLog(s, log(id));
		expect(s.logs).toHaveLength(MAX_LOG_LINES);
		expect(s.logsTruncated).toBe(false);
	});

	it('dedup still works across the cap boundary (evicted ids stay dropped)', () => {
		const s = initialState();
		for (let id = 1; id <= MAX_LOG_LINES + 3; id++) applyLog(s, log(id));
		// ids 1..3 were evicted, but maxLogId protects against their replay
		expect(applyLog(s, log(2))).toBe(false);
		expect(applyLog(s, log(MAX_LOG_LINES + 3))).toBe(false);
		expect(s.logs).toHaveLength(MAX_LOG_LINES);
		expect(applyLog(s, log(MAX_LOG_LINES + 4))).toBe(true);
		expect(s.logs[0].id).toBe(5);
	});

	it('mergeLogs evicts from the front when history overflows the cap', () => {
		const s = initialState();
		applyLog(s, log(MAX_LOG_LINES + 10)); // a live line already present
		const history = [];
		for (let id = 1; id <= MAX_LOG_LINES + 2; id++) history.push(log(id));
		mergeLogs(s, history);
		expect(s.logs).toHaveLength(MAX_LOG_LINES);
		expect(s.logsTruncated).toBe(true);
		// newest ids win: the live line survives, the oldest history is evicted
		expect(s.logs[s.logs.length - 1].id).toBe(MAX_LOG_LINES + 10);
		expect(s.logs[0].id).toBe(4);
		expect(s.maxLogId).toBe(MAX_LOG_LINES + 10);
	});
});

describe('mergeLogs (history fetch)', () => {
	it('fills the snapshot gap below maxLogId, sorted and deduped', () => {
		const s = initialState();
		applySnapshot(s, snapshot(3));
		applyLog(s, log(4)); // live line arrives before history resolves
		mergeLogs(s, [log(1), log(2), log(3)]);
		expect(s.logs.map((l) => l.id)).toEqual([1, 2, 3, 4]);
		expect(s.maxLogId).toBe(4);
	});

	it('skips lines already present', () => {
		const s = initialState();
		applyLog(s, log(1));
		applyLog(s, log(2));
		mergeLogs(s, [log(2), log(3)]);
		expect(s.logs.map((l) => l.id)).toEqual([1, 2, 3]);
	});

	it('is a no-op for an empty page', () => {
		const s = initialState();
		applyLog(s, log(5));
		mergeLogs(s, []);
		expect(s.logs.map((l) => l.id)).toEqual([5]);
	});
});

describe('applyTask', () => {
	it('replaces status/attempt and stamps started_at on running', () => {
		const s = initialState();
		applySnapshot(s, snapshot());
		applyTask(s, { task_id: 'last', status: 'running', attempt: 1 }, NOW);
		const t = s.detail!.tasks[2];
		expect(t.status).toBe('running');
		expect(t.attempt).toBe(1);
		expect(t.started_at).toBe(NOW);
		expect(t.finished_at).toBeNull();
	});

	it('stamps finished_at on terminal transition and recomputes tasks_done', () => {
		const s = initialState();
		applySnapshot(s, snapshot());
		applyTask(s, { task_id: 'fan', status: 'success', attempt: 1 }, NOW);
		const t = s.detail!.tasks[1];
		expect(t.finished_at).toBe(NOW);
		expect(s.detail!.run.tasks_done).toBe(2);
	});

	it('is idempotent: replaying a terminal event does not move finished_at', () => {
		const s = initialState();
		applySnapshot(s, snapshot());
		applyTask(s, { task_id: 'fan', status: 'success', attempt: 1 }, NOW);
		applyTask(s, { task_id: 'fan', status: 'success', attempt: 1 }, '2026-07-05T12:59:59Z');
		expect(s.detail!.tasks[1].finished_at).toBe(NOW);
	});

	it('keeps attempt on skipped (attempt 0)', () => {
		const s = initialState();
		applySnapshot(s, snapshot());
		applyTask(s, { task_id: 'fan', status: 'skipped', attempt: 0 }, NOW);
		expect(s.detail!.tasks[1].attempt).toBe(1);
	});

	it('appends a synthetic row for tasks the snapshot has not seen yet', () => {
		const s = initialState();
		applySnapshot(s, snapshot());
		applyTask(s, { task_id: 'late_task', status: 'skipped', attempt: 0 }, NOW);
		expect(s.detail!.tasks).toHaveLength(4);
		const t = s.detail!.tasks[3];
		expect(t.task_id).toBe('late_task');
		expect(t.status).toBe('skipped');
		expect(s.detail!.run.tasks_done).toBe(2);
	});
});

describe('applyRun', () => {
	it('updates status/finished_at/error and derives duration', () => {
		const s = initialState();
		applySnapshot(s, snapshot());
		applyRun(s, { status: 'failed', finished_at: '2026-07-05T12:02:00Z', error: 'boom' });
		expect(s.detail!.run.status).toBe('failed');
		expect(s.detail!.run.error).toBe('boom');
		expect(s.detail!.run.duration_sec).toBe(120);
	});

	it('non-terminal run event only swaps status', () => {
		const s = initialState();
		applySnapshot(s, snapshot());
		applyRun(s, { status: 'running' });
		expect(s.detail!.run.status).toBe('running');
		expect(s.detail!.run.finished_at).toBeNull();
	});
});

describe('orderTasks', () => {
	const def: FlowDefinition = {
		name: 'health-fan',
		namespace: 'test',
		description: '',
		inputs: [],
		variables: [],
		triggers: [],
		tasks: [
			{ id: 'first', type: 'http.request', config: {}, outputs: [] },
			{ id: 'fan', type: 'parallel', items: '{{ inputs.items }}', concurrency: 3, tasks: [], outputs: [] },
			{ id: 'last', type: 'http.request', config: {}, outputs: [] }
		]
	};

	it('adds synthetic pending rows for definition tasks without task_runs', () => {
		const started = detail().tasks.slice(0, 2); // 'last' has no row yet
		const out = orderTasks(started, def);
		expect(out.map((t) => t.task_id)).toEqual(['first', 'fan', 'last']);
		expect(out[2].status).toBe('pending');
		expect(out[2].run_id).toBe(7);
	});

	it('keeps definition order and appends rows unknown to the definition', () => {
		const rows = detail().tasks.reverse();
		const ghost = { ...detail().tasks[0], task_id: 'ghost' };
		const out = orderTasks([...rows, ghost], def);
		expect(out.map((t) => t.task_id)).toEqual(['first', 'fan', 'last', 'ghost']);
	});

	it('passes rows through untouched without a definition', () => {
		const rows = detail().tasks;
		expect(orderTasks(rows, null)).toBe(rows);
	});
});

describe('applyItems', () => {
	it('replaces the aggregate and mirrors counts into detail.fanout', () => {
		const s = initialState();
		applySnapshot(s, snapshot());
		applyItems(s, {
			task_id: 'fan',
			total: 12,
			queued: 0,
			running: 0,
			success: 11,
			failed: 1,
			dropped: 0,
			retried: 2,
			throughput_per_sec: 4.2
		});
		expect(s.itemsAgg.fan.success).toBe(11);
		expect(s.itemsAgg.fan.throughput_per_sec).toBe(4.2);
		expect(s.detail!.fanout.fan.success).toBe(11);
		expect('throughput_per_sec' in s.detail!.fanout.fan).toBe(false);
	});
});
