import { useState, forwardRef, type ReactElement, type Ref } from 'react';
import { Alert, Button, Snackbar } from '@mui/material';
import Dialog from '@mui/material/Dialog';
import DialogActions from '@mui/material/DialogActions';
import DialogContentText from '@mui/material/DialogContentText';
import DialogContent from '@mui/material/DialogContent';
import DialogTitle from '@mui/material/DialogTitle';
import Slide from '@mui/material/Slide';
import { TransitionProps } from '@mui/material/transitions';

import { isConnectionDrop, postUpdatesRollback, waitForPoddRestart } from '@api/updates.ts';


const Transition = forwardRef(function Transition(
  props: TransitionProps & {
    children: ReactElement<any, any>;
  },
  ref: Ref<unknown>,
) {
  return <Slide direction="up" ref={ ref } { ...props } />;
});

type RollbackButtonProps = {
  disabled?: boolean;
  onDone?: () => void;
};

// eslint-disable-next-line react/no-multi-comp
export default function RollbackButton({ disabled, onDone }: RollbackButtonProps) {
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [restored, setRestored] = useState<string | null>(null);

  const rollback = () => {
    setOpen(false);
    postUpdatesRollback()
      .then(result => {
        setRestored(result.restored);
        onDone?.();
      })
      .catch(async (e: unknown) => {
        // podd restarts itself on a successful rollback, so the request
        // usually dies in flight. Wait for it to come back and report the
        // release it is actually running instead of claiming a failure.
        if (!isConnectionDrop(e)) {
          console.error(e);
          const message = (e as { response?: { data?: unknown } }).response?.data;
          setError(typeof message === 'string' && message.length > 0 ? message : 'Rollback failed.');
          return;
        }
        const report = await waitForPoddRestart();
        const running = report?.updater?.currentVersions.find(v => v.kind === 'app')?.version;
        if (report === null) {
          setError('podd has not answered since the restart — reload this page in a minute.');
        } else {
          setRestored(running ?? 'the previous release');
        }
        onDone?.();
      });
  };

  return (
    <>
      <Button
        variant="outlined"
        color="warning"
        size="small"
        disabled={ disabled }
        onClick={ () => setOpen(true) }
      >
        Roll back
      </Button>
      <Dialog
        open={ open }
        slots={ { transition: Transition } }
        keepMounted
        onClose={ () => setOpen(false) }
      >
        <DialogTitle>Roll back to the previous release?</DialogTitle>
        <DialogContent>
          <DialogContentText>
            podd will switch back to the release it ran before the last update and restart.
            The bed keeps its schedule and settings; heating pauses for a few seconds while
            the daemon comes back.
          </DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button onClick={ () => setOpen(false) }>Cancel</Button>
          <Button color="warning" onClick={ rollback }>Roll back</Button>
        </DialogActions>
      </Dialog>
      <Snackbar
        open={ restored !== null }
        autoHideDuration={ 8000 }
        onClose={ () => setRestored(null) }
      >
        <Alert severity="success" onClose={ () => setRestored(null) }>
          Rolled back to { restored }; podd is restarting
        </Alert>
      </Snackbar>
      <Snackbar
        open={ error !== null }
        autoHideDuration={ 8000 }
        onClose={ () => setError(null) }
      >
        <Alert severity="error" onClose={ () => setError(null) }>
          { error }
        </Alert>
      </Snackbar>
    </>
  );
}
