"""Property-style proof that stream events reconstruct stored stream state.

This is a dependency-free model test. ``StorageModel`` is the reference for
what ``get_stream`` returns, while ``EventIndexer`` only consumes emitted
contract event payloads plus the closed-ledger timestamp carried by the RPC
event envelope. The deterministic seed makes a failure replayable.
"""

from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass, field
import random
from typing import Any


@dataclass
class StreamState:
    sender: str
    recipient: str
    token: str
    deposited: int
    withdrawn: int
    start_time: int
    end_time: int
    cliff_time: int
    flags: int
    paused_at: int | None
    paused_total: int
    status: str


@dataclass
class Event:
    kind: str
    stream_id: int
    data: dict[str, Any]
    ledger_timestamp: int


@dataclass
class StorageModel:
    now: int = 1_700_000_000
    streams: dict[int, StreamState] = field(default_factory=dict)
    events: list[Event] = field(default_factory=list)
    rng: random.Random = field(default_factory=random.Random, repr=False)

    def get_stream(self, stream_id: int) -> StreamState:
        return deepcopy(self.streams[stream_id])

    def _emit(self, kind: str, stream_id: int, data: dict[str, Any]) -> Event:
        event = Event(kind, stream_id, data, self.now)
        self.events.append(event)
        return event

    @staticmethod
    def _stream_time(stream: StreamState, now: int) -> int:
        frozen_at = stream.paused_at if stream.paused_at is not None else now
        return max(0, frozen_at - stream.paused_total)

    @classmethod
    def _vested(cls, stream: StreamState, now: int) -> int:
        if cls._stream_time(stream, now) < stream.cliff_time:
            return 0
        duration = max(0, stream.end_time - stream.start_time)
        if duration == 0:
            return stream.deposited
        consumed = max(0, min(cls._stream_time(stream, now), stream.end_time) - stream.start_time)
        if consumed >= duration:
            return stream.deposited
        return stream.deposited * consumed // duration

    def create(self, stream_id: int, sender: str, recipient: str, token: str) -> Event:
        start = self.now - self.rng.randint(0, 1_000)
        duration = self.rng.randint(5_000, 20_000)
        deposited = duration * self.rng.randint(10, 100)
        cliff = start + self.rng.randint(0, duration)
        flags = 0b111
        stream = StreamState(
            sender,
            recipient,
            token,
            deposited,
            0,
            start,
            start + duration,
            cliff,
            flags,
            None,
            0,
            "Active",
        )
        self.streams[stream_id] = stream
        return self._emit(
            "stream_created",
            stream_id,
            {
                "sender": sender,
                "recipient": recipient,
                "token": token,
                "deposited": deposited,
                "start_time": start,
                "end_time": start + duration,
                "cliff_time": cliff,
                "cancellable": True,
                "pausable": True,
                "transferable": True,
            },
        )

    def pause(self, stream_id: int) -> Event:
        stream = self.streams[stream_id]
        stream.paused_at = self.now
        stream.status = "Paused"
        return self._emit(
            "paused",
            stream_id,
            {"sender": stream.sender, "paused_at": self.now, "paused_total": stream.paused_total},
        )

    def resume(self, stream_id: int) -> Event:
        stream = self.streams[stream_id]
        paused_duration = max(0, self.now - (stream.paused_at or self.now))
        stream.paused_total += paused_duration
        stream.paused_at = None
        stream.status = "Active"
        return self._emit(
            "resumed",
            stream_id,
            {"sender": stream.sender, "paused_duration": paused_duration, "paused_total": stream.paused_total},
        )

    def top_up(self, stream_id: int, amount: int) -> Event:
        stream = self.streams[stream_id]
        duration = stream.end_time - stream.start_time
        delta = amount * duration // stream.deposited
        stream.deposited += amount
        stream.end_time += delta
        return self._emit(
            "topped_up",
            stream_id,
            {"sender": stream.sender, "amount": amount, "deposited": stream.deposited, "end_time": stream.end_time},
        )

    def withdraw(self, stream_id: int, amount: int) -> Event:
        stream = self.streams[stream_id]
        stream.withdrawn += amount
        if stream.withdrawn >= stream.deposited and stream.status != "Cancelled":
            stream.status = "Depleted"
            if stream.paused_at is not None:
                stream.paused_total += max(0, self.now - stream.paused_at)
                stream.paused_at = None
        return self._emit(
            "withdrawn",
            stream_id,
            {
                "recipient": stream.recipient,
                "amount": amount,
                "withdrawn": stream.withdrawn,
                "deposited": stream.deposited,
                "status": stream.status,
            },
        )

    def cancel(self, stream_id: int) -> Event:
        stream = self.streams[stream_id]
        vested = self._vested(stream, self.now)
        refunded = stream.deposited - vested
        stream.deposited = vested
        stream.end_time = max(stream.start_time, self._stream_time(stream, self.now))
        stream.paused_at = None
        stream.status = "Cancelled"
        return self._emit(
            "cancelled",
            stream_id,
            {
                "sender": stream.sender,
                "recipient": stream.recipient,
                "refunded": refunded,
                "vested": vested,
                "withdrawn": stream.withdrawn,
                "end_time": stream.end_time,
            },
        )

    def transfer(self, stream_id: int, recipient: str) -> Event:
        stream = self.streams[stream_id]
        old_recipient = stream.recipient
        stream.recipient = recipient
        return self._emit(
            "recipient_transferred",
            stream_id,
            {"old_recipient": old_recipient, "new_recipient": recipient},
        )

    def ttl_extended(self, stream_id: int) -> Event:
        return self._emit("ttl_extended", stream_id, {"extended_to_ledgers": 100})


@dataclass
class EventIndexer:
    streams: dict[int, StreamState] = field(default_factory=dict)

    def consume(self, event: Event) -> None:
        data = event.data
        if event.kind == "stream_created":
            self.streams[event.stream_id] = StreamState(
                sender=data["sender"],
                recipient=data["recipient"],
                token=data["token"],
                deposited=data["deposited"],
                withdrawn=0,
                start_time=data["start_time"],
                end_time=data["end_time"],
                cliff_time=data["cliff_time"],
                flags=(int(data["cancellable"]) | (int(data["pausable"]) << 1) | (int(data["transferable"]) << 2)),
                paused_at=None,
                paused_total=0,
                status="Active",
            )
            return

        stream = self.streams[event.stream_id]
        if event.kind == "withdrawn":
            stream.recipient = data["recipient"]
            stream.deposited = data["deposited"]
            stream.withdrawn = data["withdrawn"]
            stream.status = data["status"]
            # apply_withdrawal closes an in-progress pause when this payout
            # depletes the stream; the RPC event's ledger timestamp supplies now.
            if stream.status == "Depleted" and stream.paused_at is not None:
                stream.paused_total += max(0, event.ledger_timestamp - stream.paused_at)
                stream.paused_at = None
        elif event.kind == "cancelled":
            stream.sender = data["sender"]
            stream.recipient = data["recipient"]
            stream.deposited = data["vested"]
            stream.withdrawn = data["withdrawn"]
            stream.end_time = data["end_time"]
            stream.paused_at = None
            stream.status = "Cancelled"
        elif event.kind == "paused":
            stream.paused_at = data["paused_at"]
            stream.paused_total = data["paused_total"]
            stream.status = "Paused"
        elif event.kind == "resumed":
            stream.paused_total = data["paused_total"]
            stream.paused_at = None
            stream.status = "Active"
        elif event.kind == "topped_up":
            stream.deposited = data["deposited"]
            stream.end_time = data["end_time"]
        elif event.kind == "recipient_transferred":
            stream.recipient = data["new_recipient"]
        elif event.kind in {"ttl_extended", "delegate_granted", "delegate_revoked"}:
            pass
        else:
            raise AssertionError(f"unhandled event kind: {event.kind}")


def _assert_equal(storage: StorageModel, indexer: EventIndexer) -> None:
    assert set(storage.streams) == set(indexer.streams)
    for stream_id in storage.streams:
        assert indexer.streams[stream_id] == storage.get_stream(stream_id), stream_id


def test_random_event_reconstruction_matches_get_stream() -> None:
    rng = random.Random(0xE7E17)
    storage = StorageModel(rng=rng)
    indexer = EventIndexer()
    next_stream_id = 0

    for operation in range(100):
        storage.now += rng.randint(1, 2_000)
        active = [sid for sid, stream in storage.streams.items() if stream.status == "Active"]
        paused = [sid for sid, stream in storage.streams.items() if stream.status == "Paused"]
        live = [sid for sid, stream in storage.streams.items() if stream.status not in {"Cancelled", "Depleted"}]

        if not storage.streams or (len(storage.streams) < 8 and rng.random() < 0.12):
            event = storage.create(next_stream_id, f"G{next_stream_id}", f"R{next_stream_id}", "TOKEN")
            next_stream_id += 1
        else:
            choices = ["ttl"]
            if active:
                choices += ["pause", "withdraw", "transfer", "cancel"]
            if paused:
                choices += ["resume", "withdraw", "cancel"]
            if live:
                choices.append("top_up")
            action = rng.choice(choices)
            stream_id = rng.choice(active or paused or live or list(storage.streams))
            stream = storage.streams[stream_id]

            if action == "pause" and stream.status == "Active":
                event = storage.pause(stream_id)
            elif action == "resume" and stream.status == "Paused":
                event = storage.resume(stream_id)
            elif action == "top_up" and stream.status in {"Active", "Paused"} and storage._stream_time(stream, storage.now) < stream.end_time:
                rate = stream.deposited // (stream.end_time - stream.start_time)
                amount = max(1, rate * rng.randint(1, 500))
                event = storage.top_up(stream_id, amount)
            elif action == "withdraw":
                available = max(0, storage._vested(stream, storage.now) - stream.withdrawn)
                if available == 0:
                    event = storage.ttl_extended(stream_id)
                else:
                    amount = available if rng.random() < 0.25 else rng.randint(1, available)
                    event = storage.withdraw(stream_id, amount)
            elif action == "cancel" and stream.status in {"Active", "Paused"}:
                event = storage.cancel(stream_id)
            elif action == "transfer" and stream.status in {"Active", "Paused"} and stream.withdrawn < stream.deposited:
                recipient = f"R{rng.randint(100, 999)}"
                while recipient in {stream.sender, stream.recipient}:
                    recipient = f"R{rng.randint(100, 999)}"
                event = storage.transfer(stream_id, recipient)
            else:
                event = storage.ttl_extended(stream_id)

        indexer.consume(event)
        _assert_equal(storage, indexer)

    assert len(storage.events) == 100
    _assert_equal(storage, indexer)
