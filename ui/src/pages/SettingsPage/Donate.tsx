import { useState, useRef } from 'react';
import { Link, TextField, IconButton, InputAdornment, Typography, Box, Tooltip } from '@mui/material';
import ContentCopyIcon from '@mui/icons-material/ContentCopy';
import CurrencyBitcoinIcon from '@mui/icons-material/CurrencyBitcoin';
import AttachMoneyIcon from '@mui/icons-material/AttachMoney';
import Section from './Section.tsx';
import paypalIcon from './paypal.png';

export default function Donate() {
  const bitcoinAddress = 'bc1qjapkufh65gs68v2mkvrzq2ney3vnvv87jdxxg6';
  const [copySuccess, setCopySuccess] = useState(false);
  const textFieldRef = useRef<HTMLInputElement>(null);

  const handleCopy = async () => {
    try {
      if (navigator?.clipboard?.writeText) {
        await navigator.clipboard.writeText(bitcoinAddress);
      } else {
        // Fallback for browsers without clipboard API support
        const textField = textFieldRef.current;
        textField?.select();
        document.execCommand('copy');
      }
      setCopySuccess(true);
      setTimeout(() => setCopySuccess(false), 3000);
    } catch (err) {
      console.error('Failed to copy address', err);
    }
  };

  return (
    <Section title="">
      <Box
        sx={ {
          display: 'flex',
          alignItems: 'center',
          gap: 1,
          width: '100%',

          flexDirection: 'column',

        } }
      >
        <Typography variant="h6" sx={ { display: 'flex', alignItems: 'center' } }>
          Support the Project <AttachMoneyIcon fontSize="large"/>
        </Typography>
        <Typography variant="body2" color="textSecondary">
          Like the app? Don't like paying $200/year elsewhere? Buy me a drink instead!
        </Typography>
        <br />
        <Box
          sx={ {
            display: 'flex',
            alignItems: 'center',
            gap: 1,
            width: '100%',
            flexDirection: 'column',
          } }
        >
          <Link href="https://paypal.me/realfreesleep" target="_blank">
            <img src={ paypalIcon } alt="Donate via PayPal" width={ 225 } height={ 60 }/>
          </Link>
          <Link href="https://paypal.me/realfreesleep" target="_blank">
            Donate via PayPal
          </Link>
        </Box>
        <br />
        <Typography variant='h6' sx={ { display: 'flex', alignItems: 'center' } }>
          Bitcoin <CurrencyBitcoinIcon />
        </Typography>
        <Typography variant="body2" color="textSecondary">
          { copySuccess ? 'Copied!' : 'Copy and send to the bitcoin address below' }
        </Typography>
        <TextField
          inputRef={ textFieldRef }
          variant="outlined"
          fullWidth
          onSelect={ handleCopy }
          value={ bitcoinAddress }
          size='small'
          sx={ {
            cursor: 'pointer',
            '& .MuiInputBase-input': {
              cursor: 'pointer',
              fontSize: '12px',
              fontFamily: 'monospace',
            }
          } }
          inputProps={ { readOnly: true } }
          InputProps={ {
            endAdornment: (
              <InputAdornment position="end">
                <Tooltip title="Copy">
                  <IconButton onClick={ handleCopy }>
                    <ContentCopyIcon/>
                  </IconButton>
                </Tooltip>
              </InputAdornment>
            ),
          } }
        />
      </Box>
    </Section>
  );
}
