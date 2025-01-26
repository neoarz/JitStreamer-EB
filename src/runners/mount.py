# Jackson Coxson + ny

import asyncio
import aiosqlite

import netmuxd

from pymobiledevice3.services.mobile_image_mounter import auto_mount_personalized
from pymobiledevice3.tunneld.api import async_get_tunneld_device_by_udid


async def process_mount_queue():
    """
    Reads from the SQLite database and processes pending mount jobs.
    """
    db_path = "jitstreamer.db"

    async with aiosqlite.connect(db_path) as db:
        while True:
            await db.execute("BEGIN IMMEDIATE")
            # Fetch the next pending job
            async with db.execute(
                """
                SELECT udid, ip, ordinal
                FROM mount_queue
                WHERE status = 0
                ORDER BY ordinal ASC
                LIMIT 1
                """
            ) as cursor:
                row = await cursor.fetchone()

            if not row:
                await db.commit()
                await asyncio.sleep(1)
                continue

            udid, ip, ordinal = row

            # Claim the job by setting status to 1
            await db.execute(
                "UPDATE mount_queue SET status = 1 WHERE ordinal = ?",
                (ordinal,),
            )
            await db.commit()

            print(f"[INFO] Claimed mount job for UDID: {udid}, Ordinal: {ordinal}")

            try:
                if not await netmuxd.add_device(ip, udid):
                    raise Exception(f"Failed to add device to netmuxd {udid}")

                await asyncio.sleep(3)

                # Get the device
                device = None
                for _ in range(15):
                    try:
                        d = await asyncio.wait_for(
                            async_get_tunneld_device_by_udid(udid), timeout=10
                        )
                        if d:
                            device = d
                            break
                        await asyncio.sleep(1)
                    except asyncio.TimeoutError:
                        await asyncio.sleep(1)

                if not device:
                    raise RuntimeError(f"Device {udid} not found!")

                # Process the mount
                result = await auto_mount_personalized(device)
                print(f"[INFO] {result}")

                # Delete the device from the queue
                await db.execute(
                    "DELETE FROM mount_queue WHERE ordinal = ?",
                    (ordinal,),
                )
            except Exception as e:
                print(f"[ERROR] Error mounting device {udid}: {e}")

                # Update the database with the error
                await db.execute(
                    "UPDATE mount_queue SET status = 2, error = ? WHERE ordinal = ?",
                    (str(e), ordinal),
                )

            await db.commit()
            await netmuxd.remove_device(udid)
            await netmuxd.cancel_tunneld(udid)
            print(f"[INFO] Finished processing ordinal {ordinal}")


if __name__ == "__main__":
    try:
        asyncio.run(process_mount_queue())
    except KeyboardInterrupt:
        print("Shutting down gracefully...")
