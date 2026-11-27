import { useCallback, useEffect, useState } from 'react';
import { Button, Icon, Info, Modal, Switch } from '../components';
import { bytes } from '../components/FilePicker';
import { activeLanguage, fault, t } from '../i18n';
import { clock } from './MachineParts';
import { act, bag, rows, text, useOthers, type Server, type Service } from './Server';

/** One copy, as `server.copies` lists it. */
interface Made {
  copy: string;
  made: number;
  note: string;
  /** Made just before this copy was put back. */
  before: string;
  kind: string;
  made_with: string;
  machine: string;
  bytes: number;
}

interface Listed {
  paths: string[];
  /** In the machine now; null while it is off. */
  size: number | null;
  /** A database: copied while it runs. */
  running: boolean;
  copies: Made[];
  unfinished: string[];
}

/** "Today, 14:02", "Yesterday, 18:40", or the date; `inline` for the middle of a sentence: "today at 14:02". */
export function madeAt(seconds: number, inline = false): string {
  const when = new Date(seconds * 1000);
  const day = (date: Date) => new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
  const days = Math.round((day(new Date()) - day(when)) / 86_400_000);
  const time = clock(when);
  if (days === 0) return t(inline ? 'beatbox.srv.copies.today_in' : 'beatbox.srv.copies.today', { time });
  if (days === 1) return t(inline ? 'beatbox.srv.copies.yesterday_in' : 'beatbox.srv.copies.yesterday', { time });
  const locale = activeLanguage() === 'pt_PT' ? 'pt-PT' : 'en';
  const date = when.toLocaleDateString(locale, { day: 'numeric', month: 'short' });
  return inline ? t('beatbox.srv.copies.date_in', { date, time }) : `${date}, ${time}`;
}

/** A service's data and its copies, for whoever needs them. */
export function useCopies(srv: Server, service: string) {
  const [listed, setListed] = useState<Listed | null>(null);
  // It answered, and it keeps nothing: say so rather than leave a gap to wonder at.
  const [none, setNone] = useState(false);
  const load = useCallback(async () => {
    try {
      const said = await act(srv.id, 'server.copies', { service });
      const data = said.data ? bag(said.data) : null;
      setNone(!data);
      setListed(data ? {
        paths: (data.paths as string[] | undefined) ?? [],
        size: typeof data.bytes === 'number' ? data.bytes : null,
        running: data.running === true,
        copies: rows(said.copies).map((row) => ({
          copy: text(row.copy), made: Number(row.made), note: text(row.note), before: text(row.before), kind: text(row.kind),
          made_with: text(row.made_with), machine: text(row.machine), bytes: Number(row.bytes),
        })),
        unfinished: (said.unfinished as string[] | undefined) ?? [],
      } : null);
    } catch {
      setListed(null);
    }
  }, [srv.id, service]);
  useEffect(() => { void load(); }, [load]);
  return { listed, none, load };
}

/** Its data and its copies, in the service's panel; one line for a service that keeps none. */
export function Copies({ srv, service }: { srv: Server; service: Service }) {
  const { listed, none, load } = useCopies(srv, service.name);
  const machine = useMachine(srv);
  const [making, setMaking] = useState(false);
  const [back, setBack] = useState<Made | null>(null);
  // The copy whose × was pressed: gone for good, so its row asks once.
  const [dropping, setDropping] = useState('');
  const [told, setTold] = useState('');

  if (!listed) {
    return none ? (
      <div className="bb-copies">
        <div className="bb-copies-part">
          <div className="bb-copies-head"><b>{t('beatbox.srv.copies.data')}</b></div>
          <span className="bb-faint">{t('beatbox.srv.copies.nothing', { service: service.name })}</span>
        </div>
      </div>
    ) : null;
  }
  const last = listed.copies[0];
  const database = text(service.config.database) || 'postgres';

  async function drop(copy: string) {
    setDropping('');
    setTold('');
    try {
      await act(srv.id, 'server.copy_delete', { service: service.name, copy });
    } catch (error) {
      setTold(fault(error));
    }
    await load();
  }

  return (
    <div className="bb-copies">
      <div className="bb-copies-part">
        <div className="bb-copies-head"><b>{t('beatbox.srv.copies.data')}</b><Info text={t('beatbox.srv.copies.data_info', { service: service.name, machine })} /></div>
        <div className="bb-copies-row"><span className="bb-faint">{t('beatbox.srv.copies.in', { machine })}</span><span>{listed.size === null ? '—' : bytes(listed.size)}</span></div>
        {listed.running ? <div className="bb-copies-row"><span className="bb-faint">{t('beatbox.srv.copies.database')}</span><span className="bb-mono">{database}</span></div> : null}
        <div className="bb-copies-row"><span className="bb-faint">{t('beatbox.srv.copies.last')}</span><span>{last ? madeAt(last.made) : t('beatbox.srv.copies.never')}</span></div>
      </div>
      <div className="bb-copies-part">
        <div className="bb-copies-head">
          <b>{t('beatbox.srv.copies.title')}</b><Info text={t('beatbox.srv.copies.info', { service: service.name })} />
          <span style={{ flex: 1 }} />
          <Button size="sm" variant="primary" onClick={() => { setTold(''); setMaking(true); }}>{t('beatbox.srv.copies.make')}</Button>
        </div>
        {listed.unfinished.map((copy) => (
          <div key={copy} className="bb-copies-row is-half">
            <span className="bb-copies-when"><span>{t('beatbox.srv.copies.unfinished', { when: madeAt(Number(copy)) })}</span><small>{t('beatbox.srv.copies.unfinished_why')}</small></span>
            <Button size="sm" onClick={() => void drop(copy)}>{t('beatbox.srv.copies.delete_it')}</Button>
          </div>
        ))}
        {listed.copies.map((made) => dropping === made.copy ? (
          <div key={made.copy} className="bb-copies-row is-half">
            <span className="bb-copies-when"><span>{t('beatbox.srv.copies.sure_delete', { when: madeAt(made.made, true) })}</span></span>
            <Button size="sm" onClick={() => setDropping('')}>{t('beatbox.srv.copies.keep')}</Button>
            <Button size="sm" variant="danger" onClick={() => void drop(made.copy)}>{t('beatbox.srv.copies.delete_it')}</Button>
          </div>
        ) : (
          <div key={made.copy} className="bb-copies-row">
            <span className="bb-copies-when"><span>{madeAt(made.made)}</span>{noteOf(made) ? <small>{noteOf(made)}</small> : null}</span>
            <span className="bb-faint">{bytes(made.bytes)}</span>
            <Button size="sm" onClick={() => { setTold(''); setBack(made); }}>{t('beatbox.srv.copies.put_back')}</Button>
            <button type="button" className="bb-copies-x" aria-label={t('beatbox.srv.copies.delete')} title={t('beatbox.srv.copies.delete')}
                    onClick={() => setDropping(made.copy)}>
              <Icon name="close" size="sm" />
            </button>
          </div>
        ))}
        {!listed.copies.length && !listed.unfinished.length ? <p className="bb-faint" style={{ margin: 0 }}>{t('beatbox.srv.copies.empty')}</p> : null}
        {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
      </div>
      {making ? <MakeCopy srv={srv} service={service} listed={listed} database={database}
                          onClose={() => setMaking(false)} onDone={() => void load()} /> : null}
      {back ? <PutBack srv={srv} service={service} made={back} running={listed.running}
                       onClose={() => setBack(null)} onDone={async () => { await load(); await srv.load(); }} /> : null}
    </div>
  );
}

/** What the person wrote, or why BeatBox made it. */
const noteOf = (made: Made) => made.note
  || (made.before ? t('beatbox.srv.copies.before', { when: madeAt(Number(made.before), true) }) : '');

function useMachine(srv: Server) {
  const hosts = useOthers('host');
  const hostId = text(bag(srv.server?.config.uses).host);
  return hosts.find((row) => row.id === hostId)?.name ?? '';
}

function MakeCopy({ srv, service, listed, database, onClose, onDone }: {
  srv: Server; service: Service; listed: Listed; database: string; onClose: () => void; onDone: () => void;
}) {
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [told, setTold] = useState('');
  // A copy of files stops the service for a moment; a database copies itself running.
  const stops = !listed.running && service.state === 'running';

  async function make() {
    setBusy(true);
    setTold('');
    try {
      await act(srv.id, 'server.copy', { service: service.name, note });
      onDone();
      onClose();
    } catch (error) {
      setTold(fault(error));
      setBusy(false);
    }
  }

  return (
    <Modal title={t('beatbox.srv.copies.copy_title', { service: service.name })} icon="copy" width={500}
           info={t('beatbox.srv.copies.info', { service: service.name })} onClose={onClose}>
      <div className="bb-prep">
        {busy ? (
          <div className="bb-prep-status"><span className="bb-copies-spin" aria-hidden="true" /><span>{t('beatbox.srv.copies.copying')}</span></div>
        ) : (
          <>
            <div>
              <p className="bb-prep-label">{t('beatbox.srv.copies.copies_what')}</p>
              <div className="bb-prep-fact">
                <span>{listed.running ? <>{t('beatbox.srv.copies.the_database')} <span className="bb-mono">{database}</span></> : t('beatbox.srv.copies.its_data')}</span>
                {!listed.running && listed.size !== null ? <span className="bb-faint">{t('beatbox.srv.prep.about_size', { size: bytes(listed.size) })}</span> : null}
              </div>
              <div className="bb-prep-fact"><span className="bb-faint">{t('beatbox.srv.copies.kept_on')}</span><span>{t('beatbox.srv.copies.this_computer')}</span></div>
            </div>
            <label className="bb-copies-note">
              <span className="bb-prep-label">{t('beatbox.srv.copies.note')}</span>
              <input className="bb-input" value={note} maxLength={200} placeholder={t('beatbox.srv.copies.note_hint')}
                     onChange={(event) => setNote(event.target.value)} />
            </label>
            {stops ? (
              <div className="bb-prep-box is-amber"><b>{t('beatbox.srv.copies.stops', { service: service.name })}</b><span>{t('beatbox.srv.copies.stops_why')}</span></div>
            ) : null}
            {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
            <div className="bb-prep-acts">
              <Button onClick={onClose}>{t('beatbox.files.cancel')}</Button>
              <Button variant="primary" icon="copy" onClick={() => void make()}>{t('beatbox.srv.copies.make')}</Button>
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}

function PutBack({ srv, service, made, running, onClose, onDone }: {
  srv: Server; service: Service; made: Made; running: boolean;
  onClose: () => void; onDone: () => Promise<void>;
}) {
  const [first, setFirst] = useState(true);
  const [view, setView] = useState<'ask' | 'working' | 'done'>('ask');
  const [told, setTold] = useState('');
  const when = madeAt(made.made);
  const since = madeAt(made.made, true);
  const from = made.made_with ? t('beatbox.srv.copies.from_made', { version: made.made_with, machine: made.machine }) : made.machine;

  async function putBack() {
    setView('working');
    setTold('');
    try {
      await act(srv.id, 'server.copy_back', { service: service.name, copy: made.copy, first });
      await onDone();
      setView('done');
    } catch (error) {
      setTold(fault(error));
      await onDone();
      setView('ask');
    }
  }

  let body;
  if (view === 'working') {
    body = (
      <>
        <div className="bb-prep-status"><span className="bb-copies-spin" aria-hidden="true" /><span>{t('beatbox.srv.copies.putting', { service: service.name })}</span></div>
        <div className="bb-prep-acts">
          <small className="bb-faint">{t('beatbox.srv.prep.carries_on')}</small>
          <Button onClick={onClose}>{t('beatbox.srv.lib.close')}</Button>
        </div>
      </>
    );
  } else if (view === 'done') {
    body = (
      <>
        <div className="bb-prep-box is-ok bb-prep-row"><Icon name="check" size="sm" /><b>{t('beatbox.srv.copies.back_done', { service: service.name, when: since })}</b></div>
        <div className="bb-prep-acts"><Button variant="primary" onClick={onClose}>{t('beatbox.srv.lib.close')}</Button></div>
      </>
    );
  } else {
    body = (
      <>
        <div className="bb-prep-box is-amber">
          <b>{t('beatbox.srv.copies.replaced', { service: service.name })}</b>
          <span>{t(running ? 'beatbox.srv.copies.replaced_why' : 'beatbox.srv.copies.replaced_stops', { when: since, service: service.name })}</span>
        </div>
        <div>
          <p className="bb-prep-label">{t('beatbox.srv.copies.the_copy')}</p>
          <div className="bb-prep-fact"><span className="bb-faint">{t('beatbox.srv.copies.made')}</span><span>{when}{noteOf(made) ? <span className="bb-faint"> · {noteOf(made)}</span> : null}</span></div>
          <div className="bb-prep-fact"><span className="bb-faint">{t('beatbox.srv.copies.from')}</span><span>{from}</span></div>
          {running ? <div className="bb-prep-fact"><span className="bb-faint">{t('beatbox.srv.copies.password')}</span><span className="bb-faint">{t('beatbox.srv.copies.password_kept', { service: service.name })}</span></div> : null}
        </div>
        <label className="bb-copies-first">
          <Switch checked={first} onChange={setFirst} label={t('beatbox.srv.copies.first')} />
          <span>{t('beatbox.srv.copies.first')}</span>
        </label>
        {told ? <p className="bb-sbx-no" style={{ margin: 0 }}>{told}</p> : null}
        <div className="bb-prep-acts">
          <Button onClick={onClose}>{t('beatbox.files.cancel')}</Button>
          <Button variant="primary" icon="refresh" onClick={() => void putBack()}>{t('beatbox.srv.copies.do_back')}</Button>
        </div>
      </>
    );
  }

  return (
    <Modal title={view === 'ask' ? t('beatbox.srv.copies.back_title', { when: since }) : t('beatbox.srv.copies.back_working', { service: service.name })}
           icon="refresh" width={520} onClose={onClose}>
      <div className="bb-prep">{body}</div>
    </Modal>
  );
}
