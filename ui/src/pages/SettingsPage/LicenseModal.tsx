/* eslint-disable max-len */
import React, { useState } from 'react';
import { Modal, Box, Typography, Button, Link } from '@mui/material';

const LicenseModal: React.FC = () => {
  const [open, setOpen] = useState(false);

  const handleOpen = () => setOpen(true);
  const handleClose = () => setOpen(false);

  return (
    <div>
      { /* Button to Open Modal */ }
      <Button variant="contained" onClick={ handleOpen }>
        View License and Disclaimer
      </Button>

      { /* Modal Component */ }
      <Modal
        open={ open }
        onClose={ handleClose }
        aria-labelledby="license-modal-title"
        aria-describedby="license-modal-description"
      >
        <Box
          sx={ {
            position: 'absolute',
            top: '50%',
            left: '50%',
            transform: 'translate(-50%, -50%)',
            width: '80%',
            maxHeight: '80vh',
            bgcolor: 'background.paper',
            boxShadow: 24,
            p: 4,
            overflowY: 'auto',
            borderRadius: '8px',
          } }
        >
          { /* Modal Content */ }
          <Typography id="license-modal-title" variant="h6" component="h2" gutterBottom>
            Open Source Disclaimer and License
          </Typography>
          <Typography
            id="license-modal-description"
            component="pre"
            sx={ {
              whiteSpace: 'pre-wrap',
              wordWrap: 'break-word',
              overflowY: 'auto',
              maxHeight: '60vh',
              lineHeight: '1.6',
            } }
          >
            { `**Last Updated:** July 20, 2026

**1. No Affiliation or Endorsement**
podd is not affiliated with, authorized, endorsed, or supported by Eight Sleep, Inc. "Eight Sleep" and "Pod" are trademarks of Eight Sleep, Inc., used here only to identify the hardware this software runs on. Installing replacement firmware may void your device's warranty or violate Eight Sleep's terms of service.

**2. License**
podd — including this web app — is free software, licensed under the GNU General Public License, version 3 or (at your option) any later version (GPL-3.0-or-later). You are free to use, study, modify, and redistribute it under the terms of that license. The full license text is in the LICENSE file of the source repository: https://github.com/GoogleBot42/podd

This web app is derived in part from the free-sleep project (https://github.com/throwaway31265/free-sleep). The original free-sleep code is MIT licensed, and its notice is reproduced below.

**3. Use at Your Own Risk**
This software is provided "AS IS" and "WITH ALL FAULTS." By using it, you accept all risks associated with its use, including but not limited to:
- Malfunctions or disruptions to your device.
- Loss of data, settings, or functionality on your device.
- Violations of applicable laws or terms of service.

The developers disclaim all warranties, whether express or implied, including but not limited to warranties of merchantability, fitness for a particular purpose, and non-infringement.

**4. Local Operation Only**
This software is designed to operate solely on your local network. It has no cloud component: the developers provide no remote management, collect no telemetry, and cannot remotely disable or control your device. You are fully responsible for its setup, management, and operation.

**5. No Liability**
To the maximum extent permitted by law, the developers and contributors shall not be liable for any direct, indirect, incidental, special, consequential, or exemplary damages arising out of or in connection with the use of this software, including damages for loss of profits, data, or goodwill.

**6. Compliance with Laws**
You are solely responsible for ensuring that your use of this software complies with all applicable laws, regulations, and third-party agreements.

**7. Acknowledgment**
By using this software, you acknowledge that:
- You understand the risks involved in running replacement firmware on your device.
- You assume full responsibility for any consequences resulting from its use.
- You release the developers and contributors from any liability arising from your use of the software.

---

**MIT License (free-sleep portions)**

Copyright (c) 2025 the free-sleep authors (https://github.com/throwaway31265/free-sleep)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES, OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.` }
          </Typography>
          <Typography variant='body2' sx={ { mt: 2 } }>
            Full GPL-3.0 text:&nbsp;
            <Link href='https://www.gnu.org/licenses/gpl-3.0.html' target='_blank' rel='noopener noreferrer'>
              gnu.org/licenses/gpl-3.0.html
            </Link>
          </Typography>
          <Button
            variant="outlined"
            onClick={ handleClose }
            sx={ { mt: 2 } }
          >
            Close
          </Button>
        </Box>
      </Modal>
    </div>
  );
};

export default LicenseModal;
