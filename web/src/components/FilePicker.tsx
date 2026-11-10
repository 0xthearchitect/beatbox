import { useEffect, useRef, useState } from 'react';
import { Icon, type IconName } from './Icon';
import { Modal } from './Modal';
import { Button } from './primitives';
import { ChipRow } from './Burst';
import { call } from '../bridge/client';
import type { FileEntry, FolderView, Place } from '../bridge/types';
import { activeLanguage, fault, t, runtimeText } from '../i18n';

/**
 * Our own file chooser; the native one felt like leaving the app.
 *
 * Read-only on purpose: no rename, new folder, delete or copy. Nobody should
 * have to be careful in a chooser opened mid-setup.
 */
export function FilePicker({ title, suffixes, filterLabel, start, onPick, onClose, folders = false }: {
  title: string;
  /** Space-separated, as `files.list` (shell/src/files.rs) wants: ".qcow2 .iso". */
  suffixes: string;
  /** Label on the chip that turns the filter off. */
  filterLabel: string;
  /** Where to open. A file opens its folder with it selected. */
  start: string;
  onPick: (path: string) => void;
  onClose: () => void;
  /** Pick a folder: only folders listed, the open one is the answer. */
  folders?: boolean;
}) {
  const [places, setPlaces] = useState<Place[]>([]);
  const [folder, setFolder] = useState<FolderView | null>(null);
  const [where, setWhere] = useState(start);
  const [chosen, setChosen] = useState(start);
  const [filtering, setFiltering] = useState(Boolean(suffixes));
  const [hidden, setHidden] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  const list = useRef<HTMLDivElement>(null);

  useEffect(() => {
    call('files.places').then(setPlaces).catch(() => setPlaces([]));
  }, []);

  useEffect(() => {
    let live = true;
    setBusy(true);
    call('files.list', { path: where, suffixes: filtering ? suffixes : '' })
      .then((view) => {
        if (!live) return;
        setFolder(view);
        setFailure(null);
        // Otherwise a new folder opens at the old one's scroll position.
        if (list.current) list.current.scrollTop = 0;
      })
      .catch((error) => { if (live) setFailure(fault(error)); })
      .finally(() => { if (live) setBusy(false); });
    return () => { live = false; };
  }, [where, filtering, suffixes]);

  const rows = (folder?.entries ?? [])
    .filter((entry) => hidden || !entry.hidden)
    .filter((entry) => !folders || entry.folder);

  // Folder mode: the answer is wherever we are.
  useEffect(() => {
    if (folders && folder) setChosen(folder.path);
  }, [folders, folder]);

  function enter(entry: FileEntry) {
    if (entry.folder) setWhere(entry.path);
    else setChosen(entry.path);
  }

  /**
   * A typed path: a folder opens, anything else is the answer. Only the
   * backend knows which, so ask. Missing paths go through too: the caller
   * already reports "No such disk image", no need to say it twice.
   */
  async function commit(typed: string) {
    if (!typed) return;
    try {
      const view = await call('files.list', { path: typed, suffixes: filtering ? suffixes : '' });
      if (view.path === typed) { setWhere(typed); return; }
    } catch {
      /* not a folder */
    }
    onPick(typed);
  }

  return (
    <Modal title={title} icon="folder" width={860} onClose={onClose}
           footer={
             <>
               <input
                 className="bb-input bb-pick-path"
                 data-autofocus
                 aria-label={t('beatbox.files.path')}
                 value={chosen}
                 spellCheck={false}
                 placeholder={t('beatbox.files.path_placeholder')}
                 onChange={(event) => setChosen(event.target.value)}
                 onKeyDown={(event) => {
                   if (event.key === 'Enter') void commit(event.currentTarget.value.trim());
                 }}
               />
               <Button onClick={onClose}>{t('beatbox.files.cancel')}</Button>
               <Button variant="primary" disabled={!chosen.trim()}
                       onClick={() => (folders && folder && chosen.trim() === folder.path
                         ? onPick(folder.path) : void commit(chosen.trim()))}>
                 {t(folders ? 'beatbox.files.choose_folder' : 'beatbox.files.choose')}
               </Button>
             </>
           }>
      <div className="bb-pick">
        <nav className="bb-pick-places">
          {places.map((place) => (
            <button key={place.path} type="button" className="bb-pick-place"
                    aria-current={folder?.path === place.path}
                    title={place.path}
                    onClick={() => setWhere(place.path)}>
              <Icon name={place.icon as IconName} size="sm" />
              <span>{runtimeText(`beatbox.files.place.${place.key}`)}</span>
            </button>
          ))}
        </nav>

        <div className="bb-pick-main">
          <div className="bb-pick-bar">
            <Button size="sm" icon="up" disabled={!folder?.parent}
                    title={t('beatbox.files.up')}
                    onClick={() => { if (folder?.parent) setWhere(folder.parent); }}>
              {t('beatbox.files.up')}
            </Button>
            <div className="bb-pick-crumbs">
              {(folder?.crumbs ?? []).map((crumb, index, crumbs) => (
                <span key={crumb.path}>
                  {/* The top of the disk is named by its own separator, `/` or `C:\`: no second one after it. */}
                  {index && !/[\\/]$/.test(crumbs[index - 1]?.name ?? '') ? <i aria-hidden="true">/</i> : null}
                  <button type="button" onClick={() => setWhere(crumb.path)}>{crumb.name}</button>
                </span>
              ))}
            </div>
          </div>

          <div className="bb-pick-list" ref={list}>
            {failure ? <p className="bb-rose" style={{ fontSize: 12.5, padding: '14px 12px', margin: 0 }}>{failure}</p> : null}
            {!failure && !busy && !rows.length ? (
              <p className="bb-faint" style={{ fontSize: 12.5, padding: '14px 12px', margin: 0 }}>
                {t('beatbox.files.empty')}
              </p>
            ) : null}
            {rows.map((entry) => (
              <button
                key={entry.path}
                type="button"
                className={`bb-pick-row${entry.path === chosen ? ' on' : ''}${entry.hidden ? ' dim' : ''}`}
                onClick={() => enter(entry)}
                onDoubleClick={() => { if (!entry.folder) onPick(entry.path); }}
              >
                <Icon name={entry.folder ? 'folder' : 'file'} size="sm" />
                <span className="bb-pick-name">{entry.name}</span>
                <span className="bb-pick-size bb-mono">{entry.folder ? '' : bytes(entry.size)}</span>
                <span className="bb-pick-when bb-mono">{when(entry.modified)}</span>
              </button>
            ))}
          </div>

          <div className="bb-pick-foot">
            {suffixes ? (
              <ChipRow current={filtering ? 'only' : 'all'} style={{ margin: 0 }}>
                <button type="button" className="bb-chip" aria-current={filtering}
                        onClick={() => setFiltering(true)}>{filterLabel}</button>
                <button type="button" className="bb-chip" aria-current={!filtering}
                        onClick={() => setFiltering(false)}>{t('beatbox.files.all')}</button>
              </ChipRow>
            ) : <span />}
            <button type="button" className="bb-pick-toggle" onClick={() => setHidden(!hidden)}>
              {hidden ? t('beatbox.files.hide_hidden') : t('beatbox.files.show_hidden')}
            </button>
            {/* Say how many the filter hides, or people think their file vanished. */}
            {folder?.filtered && filtering ? (
              <small className="bb-faint">{t('beatbox.files.filtered', { count: folder.filtered })}</small>
            ) : null}
            {folder?.beyond ? (
              <small className="bb-faint">{t('beatbox.files.beyond', { count: folder.beyond })}</small>
            ) : null}
          </div>
        </div>
      </div>
    </Modal>
  );
}

/** Human sizes. Units are symbols, so no translation. */
function bytes(size: number): string {
  if (size < 1024) return `${size} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = size / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** Localised date, or '' if unreadable. */
function when(modified: number): string {
  if (!modified) return '';
  const locale = activeLanguage() === 'pt_PT' ? 'pt-PT' : 'en';
  return new Date(modified * 1000).toLocaleDateString(locale, {
    day: '2-digit', month: 'short', year: 'numeric',
  });
}
