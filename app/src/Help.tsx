import { useState } from "react";
import type { ReactNode } from "react";

interface HelpSection {
  id: string;
  title: string;
  body: ReactNode;
}

/**
 * Static operator guide. It works with no internet, so it lives in the app and
 * moves with the version. Messages quoted here are the ones the app shows.
 */
export function Help() {
  const [active, setActive] = useState(SECTIONS[0].id);
  const current = SECTIONS.find((section) => section.id === active) ?? SECTIONS[0];

  return (
    <div className="subpage">
      <nav className="subnav" aria-label="Guide topics">
        <p className="nav-lbl">Guide</p>
        {SECTIONS.map((section) => (
          <button
            key={section.id}
            type="button"
            className={section.id === active ? "subnav-btn is-active" : "subnav-btn"}
            aria-current={section.id === active ? "page" : undefined}
            onClick={() => setActive(section.id)}
          >
            {section.title}
          </button>
        ))}
      </nav>

      <div className="subpage-content">
        <header className="help-head">
          <p className="eyebrow">Guide</p>
          <h2>{current.title}</h2>
        </header>
        <div className="help-body">{current.body}</div>
      </div>
    </div>
  );
}

const SECTIONS: HelpSection[] = [
  {
    id: "before-doors",
    title: "Before doors open",
    body: (
      <>
        <p className="lede">
          Short answers for the desk and the gates. Everything works offline:
          scans are saved on this computer and sent when the network is back.
        </p>
        <ol>
          <li>The status bar says <strong>Online</strong> and shows the right event name.</li>
          <li>Each station shows <strong>Reader connected</strong>. If it says disconnected, check power and cable first.</li>
          <li>Do one test scan: a good ticket at the desk, a good sticker at each gate.</li>
          <li>Team lead only: the gate clock, network address and firewall are set up in advance (see <em>Setting up a gate</em>).</li>
        </ol>
      </>
    ),
  },
  {
    id: "registration-desk",
    title: "Registration desk",
    body: (
      <>
        <ol>
          <li>Scan the ticket QR code. The cursor stays in <strong>Ticket code</strong>, so the scanner always works.</li>
          <li><strong>Ticket found</strong> appears with the guest name. The guest is checked in.</li>
          <li>Put a sticker or wristband on the reader. <strong>Linked</strong> means the sticker now belongs to that ticket.</li>
          <li>The badge prints by itself the first time a guest checks in, when the computer is online.</li>
        </ol>
        <dl>
          <dt>Guest has no QR code</dt>
          <dd>Use <strong>Find a ticket</strong> and type a name, email address or phone number. Offline, only names can be searched.</dd>
          <dt>Sticker did not link</dt>
          <dd>Press <strong>Try the sticker again</strong>. Check the sticker sits flat on the reader.</dd>
          <dt>Badge did not print</dt>
          <dd>Open <strong>Printer</strong>, check the printer name and press <strong>Test print</strong>. If Setup → <strong>Badge printing</strong> is set to the event-printing app, check that app is running. Press <strong>Reprint badge</strong> only if a badge is still needed.</dd>
          <dt>Wrong ticket scanned</dt>
          <dd>Press <strong>Cancel and start over</strong>.</dd>
          <dt>Guest lost or damaged the sticker</dt>
          <dd>Scan the ticket again and link a new sticker. The app asks <strong>Replace this sticker?</strong> and needs a reason. The old sticker stops working.</dd>
          <dt>Mode: Bind or Write</dt>
          <dd>Set by the event on the server. Bind links a sticker that already has a code. Write also writes the ticket onto the sticker.</dd>
        </dl>
      </>
    ),
  },
  {
    id: "verify",
    title: "Checking a sticker (Verify)",
    body: (
      <>
        <p>
          Use <strong>Verify</strong> when a guest asks whether their sticker works, or to check whose
          sticker you are holding. It only looks: it never links, checks in or prints.
        </p>
        <ol>
          <li>Open the <strong>Verify</strong> tab in the navigation. It needs the internet, because the name comes from the server.</li>
          <li>Hold the sticker near the reader. The guest&apos;s name fills the screen for a few seconds.</li>
          <li>Take the sticker away. The screen goes back to waiting for the next one.</li>
        </ol>
        <dl>
          <dt>Green with a name</dt>
          <dd>The sticker belongs to that guest and the ticket is valid.</dd>
          <dt>Red with a name</dt>
          <dd>The sticker is linked, but the ticket is not valid (unpaid or cancelled). Send the guest to registration.</dd>
          <dt>Yellow: not linked to any ticket</dt>
          <dd>Nobody has linked this sticker. Send the guest to the registration desk.</dd>
          <dt>Grey message</dt>
          <dd>Hold one sticker at a time, check the reader is connected, or wait for the connection to return.</dd>
        </dl>
      </>
    ),
  },
  {
    id: "gates",
    title: "Entry and exit gates",
    body: (
      <>
        <p>
          A guest walks through, the gate reads the sticker, and the screen shows the name and
          <strong> Welcome</strong> (entry) or <strong>Goodbye</strong> (exit). The heading over the name
          says which direction this gate is recording.
        </p>
        <h4>Switching a gate between Entry and Exit</h4>
        <p>Takes seconds. Nothing restarts, the reader stays connected and no scan is lost.</p>
        <ol>
          <li>Open <strong>Setup</strong>.</li>
          <li>Change <strong>Direction</strong> on that gate to Entry or Exit.</li>
          <li>Press <strong>Save setup</strong>, then <strong>Change it and save</strong> in the box that asks <em>Change a gate direction?</em></li>
        </ol>
        <p>
          Passes scanned before the switch keep the direction they had when scanned.
          Gates need RfiDex 0.6.16 or newer for the instant switch.
        </p>
        <div className="warn">
          Do not press <strong>Read gate records</strong> in Setup while the event is running.
          It is a test button: it takes the next pass off the gate and does not save it.
        </div>
      </>
    ),
  },
  {
    id: "gate-screen",
    title: "What the gate screen says",
    body: (
      <>
        <table>
          <thead>
            <tr><th>You see</th><th>It means</th><th>Do</th></tr>
          </thead>
          <tbody>
            <tr><td>Welcome / Goodbye</td><td>Pass accepted</td><td>Nothing</td></tr>
            <tr><td>Recorded — waiting for the server</td><td>Saved here; the server has not answered yet</td><td>Nothing. It sends by itself. Do not scan again.</td></tr>
            <tr><td>Sticker not recognised.</td><td>Not linked to any ticket</td><td>Send the guest to registration</td></tr>
            <tr><td>Check in at registration first.</td><td>Ticket not checked in yet</td><td>Send the guest to registration</td></tr>
            <tr><td>This sticker has been replaced.</td><td>The guest has a newer sticker</td><td>Ask for the new sticker</td></tr>
            <tr><td>This sticker belongs to another event.</td><td>Wrong event</td><td>Check the guest</td></tr>
            <tr><td>This ticket cannot be used.</td><td>Ticket is cancelled or invalid</td><td>Send to registration</td></tr>
            <tr><td>This attendee has not checked in at registration.</td><td>Warning only: the pass is still recorded</td><td>Note it, no action</td></tr>
          </tbody>
        </table>
        <p>
          A red light and buzzer on the gate means the server confirmed a declined pass (sticker not
          linked to a ticket, or ticket invalid). A guest who shows as a pass on the Verify screen never
          sets it off. If the server cannot be reached in time, the gate stays silent and the pass is
          still recorded, so watch the Problems tab. Read the message on screen before letting the guest through.
        </p>
      </>
    ),
  },
  {
    id: "offline",
    title: "Offline and sending",
    body: (
      <ul>
        <li>The status bar shows <strong>Online</strong> or <strong>Offline at n station(s)</strong>, and how many actions are <strong>waiting to send</strong>.</li>
        <li>Offline is fine. Keep scanning. Everything is saved on this computer and sent when the network returns.</li>
        <li>Do not scan the same guest twice to &ldquo;force it&rdquo;. Do not restart the app to fix offline.</li>
        <li><strong>Sync now</strong> (top right) tries to send everything immediately.</li>
        <li>Keep the computer on and plugged in until it says <strong>0 waiting to send</strong>.</li>
      </ul>
    ),
  },
  {
    id: "problems",
    title: "Problems tab",
    body: (
      <>
        <p>
          The <strong>Problems</strong> tab lists saved actions the server refused or the app could not send.
          The number in the tab is how many need attention. Ask the team lead before doing anything about them.
        </p>
        <p>
          <strong>Dismiss from this list</strong> only hides a row here. It does not fix the server&rsquo;s answer,
          delete the saved action, or change who holds a sticker.
        </p>
      </>
    ),
  },
  {
    id: "setup-gate",
    title: "Setting up a gate (team lead)",
    body: (
      <ol>
        <li>
          <strong>Event server.</strong> Setup → <em>Event server</em>: enter the Server URL and the event API key,
          then press <strong>Test connection</strong>. The key is never shown again after saving.
        </li>
        <li>
          <strong>Add the station.</strong> <em>Stations on this computer</em> → add a station, choose Type
          <em> Gate</em>, a name, and the Direction (Entry or Exit).
        </li>
        <li>
          <strong>Network address.</strong> The gate PC needs a fixed address on the gate&rsquo;s network
          (for example gate <code>192.168.1.222</code>, PC <code>192.168.1.50</code>, mask
          <code> 255.255.255.0</code>) and Windows Firewall must allow <code>rfidex.exe</code>.
        </li>
        <li>
          <strong>Interface</strong> is this computer&rsquo;s own address on the cable to the gate.
          <strong> Look for network cards</strong> lists this computer&rsquo;s network addresses. Pick the one on the
          gate&rsquo;s network with <em>Use this one</em>.
        </li>
        <li>
          <strong>Gate IP address</strong> is the gate&rsquo;s own IP (for example <code>192.168.1.222</code>). The
          <strong> Port</strong> is already filled in as 6688, the vendor default; leave it unless someone changed it
          on the gate. <strong>Find gates</strong> broadcasts on the chosen network card and lists gates that answer;
          press <em>Use this one</em> to fill the IP in. It only finds gates that reply to the vendor&rsquo;s discovery
          call, so if <em>No gate answered</em> appears, check power, cable and firewall, then just type the IP
          printed in the vendor tool or on the gate.
        </li>
        <li>
          <strong>Repeat window</strong> (seconds) and <strong>Alarm</strong> (all panels or only the panel that read the
          sticker) are per gate. 5 seconds suits most gates.
        </li>
        <li>Press <strong>Save setup</strong>. The station should show <strong>Reader connected</strong> within a few seconds.</li>
        <li>
          <strong>Gate clock.</strong> Open the vendor D-tool, connect to the gate and run <em>Update system time</em>.
          A wrong gate clock does not change counts here, but it makes the gate&rsquo;s own record times wrong.
        </li>
      </ol>
    ),
  },
  {
    id: "setup-desk",
    title: "Setting up the desk (team lead)",
    body: (
      <ol>
        <li>Add a station with Type <em>Desk</em> and choose the Reader (leave the DLL path empty if <code>ECRFID.dll</code> is in the RfiDex folder).</li>
        <li>Badge printing starts on the <strong>event-printing app</strong> (the current method). Keep that app running and its <strong>event-printing address</strong> under Stations for rollback.</li>
        <li>To use built-in printing, open <strong>Printer</strong>, choose the printer and badge layout, press <strong>Test print</strong> and check the badge, then pick <strong>Built-in</strong> under Setup → <strong>Badge printing</strong>. Switching back is instant and never restarts the readers; a badge already sent to the Windows print queue may still print.</li>
        <li>Writing stickers is off until the <em>Sticker write test</em> passes with a spare sticker.</li>
      </ol>
    ),
  },
  {
    id: "trouble",
    title: "If something goes wrong",
    body: (
      <dl>
        <dt>Reader disconnected</dt>
        <dd>Check power and the cable. Wait 30 seconds before touching Setup. The station reconnects by itself.</dd>
        <dt>&ldquo;The server did not accept the API key&rdquo;</dt>
        <dd>The key is wrong or was replaced. Call the team lead; do not change it yourself.</dd>
        <dt>&ldquo;This API key belongs to a different event&rdquo;</dt>
        <dd>Enter the key for this event in Setup. Saved scans stay safe and are not sent to the wrong event.</dd>
        <dt>Computer clock warning</dt>
        <dd>Fix the computer date and time. Scans use this clock.</dd>
        <dt>Low disk space</dt>
        <dd>Free some space on this computer before continuing.</dd>
        <dt>Wrong direction after a break</dt>
        <dd>Switch it again as above. Staff can also correct past direction in the panel.</dd>
        <dt>Still stuck</dt>
        <dd>Press <strong>Export diagnostics</strong> (top right) and send the file to the team. It never contains the API key.</dd>
      </dl>
    ),
  },
  {
    id: "updates",
    title: "Updates",
    body: (
      <p>
        Setup → <em>About &amp; updates</em> shows the installed version. When a newer one is ready, press
        <strong> Update now</strong>. Do it before doors open, not during the event.
      </p>
    ),
  },
];
