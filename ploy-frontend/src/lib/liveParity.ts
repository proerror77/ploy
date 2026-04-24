import type {
  OrderSnapshot,
  TradingIntentSnapshot,
  TradingStateSnapshot,
} from '@/types/operator-contracts';

type RuntimeBucket = 'dryrun' | 'live';

export interface SnapshotSummary {
  activeOrders: number;
  fills: number;
  grossExposure: string;
  intents: number;
  orders: number;
  positions: number;
  reservedExposure: string;
  totalExposure: string;
}

export interface ParityOrderRow {
  createdAt: string | null;
  limitPrice: string | null;
  orderId: string;
  quantity: string;
  side: string;
  state: string;
  tokenId: string;
}

export interface LiveParityPair {
  dryrun?: TradingStateSnapshot;
  dryrunOnlyOrders: ParityOrderRow[];
  dryrunSummary: SnapshotSummary;
  key: string;
  live?: TradingStateSnapshot;
  liveSummary: SnapshotSummary;
  message: string;
  status: 'idle' | 'matched' | 'alert' | 'missing_dryrun' | 'missing_live';
}

export interface LiveParityReport {
  alertPairs: LiveParityPair[];
  dryrunOrders: number;
  liveOrders: number;
  pairs: LiveParityPair[];
  unmatchedDryrunOrders: number;
}

const EMPTY_SUMMARY: SnapshotSummary = {
  activeOrders: 0,
  fills: 0,
  grossExposure: '0',
  intents: 0,
  orders: 0,
  positions: 0,
  reservedExposure: '0',
  totalExposure: '0',
};

function runtimeBucket(snapshot: TradingStateSnapshot): RuntimeBucket | null {
  const mode = snapshot.runtime_mode.toLowerCase();
  const id = snapshot.deployment_id.toLowerCase();

  if (mode.includes('dry')) {
    return 'dryrun';
  }

  if (mode === 'live' || id.endsWith('.live') || id.endsWith('-live')) {
    return 'live';
  }

  return null;
}

export function parityKey(snapshot: TradingStateSnapshot) {
  return snapshot.deployment_id
    .toLowerCase()
    .replace(/([._-])(dry[-_]?run|dryrun|live)$/u, '')
    .replace(/([._-])$/u, '');
}

function summarize(snapshot?: TradingStateSnapshot): SnapshotSummary {
  if (!snapshot) {
    return EMPTY_SUMMARY;
  }

  return {
    activeOrders: snapshot.risk.active_orders,
    fills: snapshot.fills.length,
    grossExposure: snapshot.risk.gross_exposure,
    intents: snapshot.intents.length,
    orders: snapshot.orders.length,
    positions: snapshot.positions.length,
    reservedExposure: snapshot.risk.reserved_order_exposure,
    totalExposure: snapshot.risk.total_gross_exposure,
  };
}

function intentById(snapshot: TradingStateSnapshot) {
  return new Map(snapshot.intents.map((intent) => [intent.intent_id, intent]));
}

function orderRow(
  order: OrderSnapshot,
  intent: TradingIntentSnapshot | undefined
): ParityOrderRow {
  return {
    createdAt: intent?.created_at ?? null,
    limitPrice: order.limit_price ?? null,
    orderId: order.order_id,
    quantity: order.requested_qty,
    side: intent?.side ?? 'unknown',
    state: order.state,
    tokenId: order.token_id,
  };
}

function orderCompareKey(
  order: OrderSnapshot,
  intent: TradingIntentSnapshot | undefined
) {
  return `${order.token_id}:${(intent?.side ?? 'unknown').toLowerCase()}`;
}

function dryrunOnlyOrders(dryrun?: TradingStateSnapshot, live?: TradingStateSnapshot) {
  if (!dryrun) {
    return [];
  }

  const dryrunIntents = intentById(dryrun);
  const liveIntents = live ? intentById(live) : new Map<string, TradingIntentSnapshot>();
  const liveOrderKeys = new Set(
    (live?.orders ?? []).map((order) =>
      orderCompareKey(order, liveIntents.get(order.intent_id))
    )
  );

  return dryrun.orders
    .filter(
      (order) =>
        !liveOrderKeys.has(orderCompareKey(order, dryrunIntents.get(order.intent_id)))
    )
    .map((order) => orderRow(order, dryrunIntents.get(order.intent_id)));
}

function pairMessage(pair: Pick<LiveParityPair, 'dryrun' | 'dryrunOnlyOrders' | 'live'>) {
  if (!pair.dryrun) {
    return 'No dry-run snapshot';
  }

  if (!pair.live) {
    return 'No live snapshot';
  }

  if (pair.dryrunOnlyOrders.length > 0) {
    return 'Dry-run order is missing from live';
  }

  if (pair.dryrun.orders.length === 0 && pair.live.orders.length === 0) {
    return 'No orders yet';
  }

  return 'Order paths match';
}

function pairStatus(pair: Pick<LiveParityPair, 'dryrun' | 'dryrunOnlyOrders' | 'live'>) {
  if (!pair.dryrun) {
    return 'missing_dryrun' as const;
  }

  if (!pair.live) {
    return pair.dryrun.orders.length > 0 ? ('alert' as const) : ('missing_live' as const);
  }

  if (pair.dryrunOnlyOrders.length > 0) {
    return 'alert' as const;
  }

  if (pair.dryrun.orders.length === 0 && pair.live.orders.length === 0) {
    return 'idle' as const;
  }

  return 'matched' as const;
}

export function buildLiveParityReport(
  snapshots: TradingStateSnapshot[] = []
): LiveParityReport {
  const grouped = new Map<string, Partial<Record<RuntimeBucket, TradingStateSnapshot>>>();

  for (const snapshot of snapshots) {
    const bucket = runtimeBucket(snapshot);
    if (!bucket) {
      continue;
    }

    const key = parityKey(snapshot);
    const current = grouped.get(key) ?? {};
    current[bucket] = snapshot;
    grouped.set(key, current);
  }

  const pairs = Array.from(grouped.entries())
    .map(([key, bucket]) => {
      const dryrun = bucket.dryrun;
      const live = bucket.live;
      const missingOrders = dryrunOnlyOrders(dryrun, live);
      const pair = {
        dryrun,
        dryrunOnlyOrders: missingOrders,
        dryrunSummary: summarize(dryrun),
        key,
        live,
        liveSummary: summarize(live),
      };

      return {
        ...pair,
        message: pairMessage(pair),
        status: pairStatus(pair),
      };
    })
    .sort((a, b) => a.key.localeCompare(b.key));

  return {
    alertPairs: pairs.filter((pair) => pair.status === 'alert'),
    dryrunOrders: pairs.reduce((sum, pair) => sum + pair.dryrunSummary.orders, 0),
    liveOrders: pairs.reduce((sum, pair) => sum + pair.liveSummary.orders, 0),
    pairs,
    unmatchedDryrunOrders: pairs.reduce(
      (sum, pair) => sum + pair.dryrunOnlyOrders.length,
      0
    ),
  };
}
