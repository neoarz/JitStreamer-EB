# Jackson Coxson + ny

import asyncio
import logging
import aiosqlite
import socket
from pymobiledevice3.services.dvt.instruments.process_control import ProcessControl
from pymobiledevice3.services.dvt.dvt_secure_socket_proxy import (
    DvtSecureSocketProxyService,
)
from pymobiledevice3.tunneld.api import async_get_tunneld_device_by_udid

logging.basicConfig(level=logging.INFO)


async def launch_app(udid, bundle_id):
    """
    Launches the app and enables JIT. Returns a success message or raises an error.
    """
    device = await async_get_tunneld_device_by_udid(udid)
    if not device:
        raise RuntimeError(f"Device {udid} not found!")

    try:
        with DvtSecureSocketProxyService(lockdown=device) as dvt:
            process_control = ProcessControl(dvt)
            app = process_control.launch(
                bundle_id=bundle_id,
                arguments={},
                kill_existing=False,
                start_suspended=True,
                environment={},
            )

            try:
                debugserver = (host, port) = (
                    device.service.address[0],
                    device.get_service_port("com.apple.internal.dt.remote.debugproxy"),
                )
            except Exception as e:
                raise RuntimeError(
                    f"Error getting debugserver address: {str(e)}, is tunneld running?"
                )
            logging.info(f"Connecting to [{host}]:{port}")

            with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as s:
                s.connect(debugserver)
                s.sendall(b"$QStartNoAckMode#b0")
                s.sendall(b"$QSetDetachOnError:1#f8")

                s.sendall(f"$vAttach;{app:x}#38".encode())
                out = s.recv(16).decode()

                if out.startswith("$T11thread") or "+" in out:
                    s.sendall(b"$D#44")
                    new = s.recv(16)
                    if any(x in new for x in (b"$T11thread", b"$OK#00", b"+")):
                        return
                    else:
                        raise Exception(f"Failed to detach process {app}")
                else:
                    raise Exception(f"Failed to attach process {app}")
    except Exception as e:
        raise RuntimeError(f"Error launching app {bundle_id} on {udid}: {str(e)}")


async def process_launch_queue():
    """
    Reads from the SQLite database and processes pending app launches.
    """
    db_path = "jitstreamer.db"

    async with aiosqlite.connect(db_path) as db:
        while True:
            # Begin a transaction to claim a pending job
            async with db.execute(
                """
                SELECT udid, bundle_id, ordinal
                FROM launch_queue
                WHERE status = 0
                ORDER BY ordinal ASC
                LIMIT 1
                """
            ) as cursor:
                row = await cursor.fetchone()

            if not row:
                logging.info("No pending launches. Retrying in 1 second...")
                await asyncio.sleep(1)
                continue

            udid, bundle_id, ordinal = row

            # Lock the job by setting the status to 1 (in progress)
            await db.execute(
                "UPDATE launch_queue SET status = 1 WHERE ordinal = ?",
                (ordinal,),
            )
            await db.commit()

            logging.info(
                f"Claimed launch job for UDID: {udid}, Bundle ID: {bundle_id}, Ordinal: {ordinal}"
            )

            try:
                # Process the launch
                result = await launch_app(udid, bundle_id)
                logging.info(result)

                # Delete the device from the queue
                await db.execute(
                    "DELETE FROM launch_queue WHERE ordinal = ?",
                    (ordinal,),
                )
            except Exception as e:
                logging.error(str(e))

                # Update the database with the error
                await db.execute(
                    "UPDATE launch_queue SET status = 2, error = ? WHERE ordinal = ?",
                    (str(e), ordinal),
                )

            await db.commit()
            logging.info(f"Finished processing ordinal {ordinal}")


if __name__ == "__main__":
    try:
        asyncio.run(process_launch_queue())
    except KeyboardInterrupt:
        logging.info("Shutting down gracefully...")
