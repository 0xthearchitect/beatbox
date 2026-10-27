/**
 * The job in flight, for the screen waiting on one.
 *
 * Ask once, then listen. Listening alone misses a job that was already
 * running when we mounted: a disk build goes quiet for the whole install, so
 * leaving the tab and coming back lit Start up again and the click got
 * refused. Don't drop the initial ask.
 */

import { useEffect, useState } from 'react';
import { call, on } from './bridge/client';
import type { Job } from './bridge/types';

export function useJob(): Job | null {
  const [job, setJob] = useState<Job | null>(null);
  useEffect(() => {
    let live = true;
    call('jobs.current').then((value) => { if (live) setJob(value); }).catch(() => undefined);
    const stop = on('job', (value) => { if (live) setJob(value); });
    return () => { live = false; stop(); };
  }, []);
  return job;
}
