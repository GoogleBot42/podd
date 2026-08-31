import { useState } from 'react';
import {
  Alert,
  Box,
  Button,
  Chip,
  CircularProgress,
  FormControl,
  InputLabel,
  MenuItem,
  Select,
  Typography,
} from '@mui/material';
import InfoIcon from '@mui/icons-material/Info';

import Section from '../Section.tsx';
import ApplyButton from './ApplyButton.tsx';
import RollbackButton from './RollbackButton.tsx';
import { ComponentKind, postUpdatesChannel, postUpdatesCheck, useUpdates } from '@api/updates.ts';
import { UI_VERSION } from '@lib/version.ts';


// The update tiers of REPLACEMENT_PLAN §9, in the order they matter to a user.
const TIER_LABELS: Record<ComponentKind, string> = {
  app: 'App (podd + UI)',
  os: 'OS image',
  mcu_frozen: 'Frozen MCU',
  mcu_sensor: 'Sensor MCU',
  bootloader: 'Bootloader',
};

// Channels podd releases are cut on (docs/RELEASING.md). The device follows
// exactly one: PODD_UPDATER_CHANNEL is the install-time default, and switching
// it here persists an override on the Pod that outranks it.
const KNOWN_CHANNELS = ['stable', 'beta'];

const formatCheckedAt = (unixSeconds: number | null) => {
  if (unixSeconds === null) return 'never';
  return new Date(unixSeconds * 1000).toLocaleString();
};

export default function UpdatesSection() {
  const { data, isLoading, refetch } = useUpdates();
  const [checking, setChecking] = useState(false);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [switchingChannel, setSwitchingChannel] = useState(false);

  if (isLoading || !data) {
    return (
      <Section title='Updates'>
        <CircularProgress />
      </Section>
    );
  }

  const updater = data.updater;

  const checkNow = () => {
    setChecking(true);
    setCheckError(null);
    postUpdatesCheck()
      .then(() => refetch())
      .catch((e: unknown) => {
        console.error(e);
        setCheckError('Update check failed — see the updater status below.');
        return refetch();
      })
      .finally(() => setChecking(false));
  };

  // Switching channels applies nothing on its own — the agent persists the
  // choice and the following check is what consults the new channel.
  const switchChannel = (next: string) => {
    setSwitchingChannel(true);
    setCheckError(null);
    postUpdatesChannel(next)
      .then(() => refetch())
      .catch((e: unknown) => {
        console.error(e);
        setCheckError(`Could not switch to the ${next} channel.`);
        return refetch();
      })
      .finally(() => setSwitchingChannel(false));
  };

  // The App tier is only known to the agent once a release was installed
  // through it; on a hand-installed device fall back to the running build.
  const versions = updater?.currentVersions ?? [];
  const hasAppVersion = versions.some(entry => entry.kind === 'app');
  const channel = updater?.channel ?? 'stable';
  const channels = KNOWN_CHANNELS.includes(channel) ? KNOWN_CHANNELS : [...KNOWN_CHANNELS, channel];
  // Only the app tier can be applied from here; OS/MCU live applies are still
  // gated stubs on the daemon side (issue #43).
  const appUpdate = updater?.available.find(component => component.kind === 'app');

  return (
    <Section title='Updates'>
      <Box display='flex' flexDirection='column' gap={ 2 }>

        <Box>
          <Typography variant='body2' sx={ { mb: 1 } }>Installed</Typography>
          <Box display='flex' gap={ 1 } flexWrap='wrap'>
            {
              !hasAppVersion && (
                <Chip label={ `${TIER_LABELS.app}: ${data.daemon.version} (${data.daemon.rev})` } size='small'/>
              )
            }
            {
              versions.map(entry => (
                <Chip
                  key={ entry.kind }
                  label={ `${TIER_LABELS[entry.kind]}: ${entry.version}` }
                  size='small'
                />
              ))
            }
            <Chip label={ `UI: ${UI_VERSION}` } size='small' variant='outlined'/>
          </Box>
        </Box>

        {
          !updater && (
            <Alert severity='info'>
              No update agent is running, so podd can't check for or roll back releases.
              Set <code>PODD_UPDATER_*</code> in a systemd drop-in to enable it — see docs/UPDATING.md.
            </Alert>
          )
        }

        {
          updater && (
            <>
              <Box display='flex' gap={ 1 } alignItems='center' flexWrap='wrap'>
                <Typography variant='body2'>Agent</Typography>
                <Chip
                  label={ updater.enabled ? 'enabled' : 'disabled' }
                  size='small'
                  color={ updater.enabled ? 'success' : 'default' }
                />
                <Chip label={ `${updater.mode} mode` } size='small'/>
                <Chip
                  label={ `checked: ${formatCheckedAt(updater.lastCheckUnix)}` }
                  size='small'
                  color={ updater.lastCheckUnix !== null && !updater.lastCheckOk ? 'warning' : 'default' }
                />
              </Box>

              {
                !updater.enabled && (
                  <Alert severity='warning'>
                    The update agent is switched off (<code>PODD_UPDATER_ENABLED=false</code>), so
                    podd won't check for or install releases and the update button stays disabled.
                    Turn it on in the systemd drop-in — see docs/UPDATING.md.
                  </Alert>
                )
              }

              <FormControl size='small' sx={ { maxWidth: 260 } }>
                <InputLabel id='update-channel-label'>Release channel</InputLabel>
                {
                  // Switching persists an override on the Pod (it survives a
                  // restart and outranks PODD_UPDATER_CHANNEL); it applies
                  // nothing — the next check is what looks at the new channel.
                }
                <Select
                  labelId='update-channel-label'
                  label='Release channel'
                  value={ channel }
                  disabled={ switchingChannel }
                  onChange={ event => switchChannel(event.target.value) }
                >
                  {
                    channels.map(name => (
                      <MenuItem key={ name } value={ name }>{ name }</MenuItem>
                    ))
                  }
                </Select>
              </FormControl>

              {
                updater.available.length > 0 && (
                  <Alert severity='info'>
                    <Typography variant='body2'>Available on { channel }:</Typography>
                    {
                      updater.available.map(component => (
                        <Typography key={ `${component.kind}-${component.name}` } variant='body2'>
                          { TIER_LABELS[component.kind] } { component.version }
                        </Typography>
                      ))
                    }
                    {
                      updater.available.some(component => component.kind !== 'app') && (
                        <Typography variant='body2' sx={ { mt: 1 } }>
                          Only the app tier can be installed from here; apply an OS or MCU
                          update from the Pod (docs/UPDATING.md).
                        </Typography>
                      )
                    }
                  </Alert>
                )
              }

              {
                updater.lastApplied && (
                  <Typography variant='body2' color='text.secondary'>
                    Last applied: { updater.lastApplied }
                  </Typography>
                )
              }

              {
                updater.lastError && (
                  <Alert severity='warning'>{ updater.lastError }</Alert>
                )
              }

              {
                checkError && (
                  <Alert severity='error' onClose={ () => setCheckError(null) }>{ checkError }</Alert>
                )
              }

              <Box display='flex' gap={ 1 } alignItems='center' flexWrap='wrap'>
                <Button
                  variant='outlined'
                  size='small'
                  disabled={ checking }
                  onClick={ checkNow }
                >
                  { checking ? 'Checking…' : 'Check now' }
                </Button>
                {
                  appUpdate && (
                    <ApplyButton
                      version={ appUpdate.version }
                      // An agent that was switched off must not install
                      // anything — the daemon refuses too, this just says so
                      // before the round-trip.
                      disabled={ !updater.enabled }
                      onDone={ () => void refetch() }
                    />
                  )
                }
                <RollbackButton onDone={ () => void refetch() }/>
              </Box>
            </>
          )
        }

        <Box display='flex' gap={ 1 }>
          <InfoIcon sx={ { color: 'text.secondary' } }/>
          <Typography variant='body2' color='text.secondary'>
            App updates are atomic and health-checked: installing one restarts podd into the new
            release as a canary, and a release that doesn't come up healthy is rolled back on its
            own. In auto mode the agent installs them without being asked. Roll back manually to
            return to the previous release — podd restarts to do it.
          </Typography>
        </Box>

      </Box>
    </Section>
  );
}
