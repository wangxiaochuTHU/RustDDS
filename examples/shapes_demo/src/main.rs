extern crate atosdds;
extern crate serde;
extern crate mio;
extern crate mio_extras;
extern crate byteorder;
extern crate termion;

use atosdds::{
  serialization::{
    cdrSerializer::CDR_serializer_adapter, cdrDeserializer::CDR_deserializer_adapter,
  },
  dds::{
    typedesc::TypeDesc,
    participant::DomainParticipant,
    qos::QosPolicies,
    datareader::DataReader,
    readcondition::ReadCondition,
    interfaces::{IDataWriter, IKeyedDataWriter, IKeyedDataReader},
  },
  dds::qos::policy::Reliability,
  structure::duration::Duration,
  dds::qos::policy::History,
  dds::qos::policy::Ownership,
  dds::qos::policy::Durability,
  dds::qos::policy::Liveliness,
  dds::qos::policy::LivelinessKind,
  dds::qos::policy::DestinationOrder,
  dds::qos::policy::ResourceLimits,
  dds::qos::policy::Deadline,
  dds::qos::policy::LatencyBudget,
  dds::qos::policy::Presentation,
  dds::qos::policy::PresentationAccessScope,
  dds::qos::policy::Lifespan,
  dds::traits::key::Keyed,
};
use std::{
  sync::{
    atomic::{Ordering, AtomicBool},
    Arc,
  },
  time::Duration as StdDuration,
};
use mio::{Poll, Token, Ready, PollOpt, Events};
use mio_extras::{timer::Timer, channel as mio_channel};
use shapes::Square;
use std::io::{Write, Read};
use termion::raw::IntoRawMode;
use byteorder::LittleEndian;

mod shapes;

fn main() {
  env_logger::init();

  let domain_id = std::env::args().nth(1).unwrap_or(String::from("0"));
  let domain_id = domain_id.parse::<u16>().unwrap();

  let (stop_channel_sender, stop_channel_receiver) = mio_channel::sync_channel(10);
  let _jhandle = std::thread::spawn(move || event_loop(stop_channel_receiver, domain_id));

  stop_control(stop_channel_sender);
}

// milliseconds
const KEYBOARD_CHECK_TIMEOUT: u64 = 50;

// declaring event loop tokens for better readability
const STOP_EVENT_LOOP_TOKEN: Token = Token(1000);
const SQUARE_READER_TOKEN: Token = Token(1001);
const KEYBOARD_CHECK_TOKEN: Token = Token(1002);

fn event_loop(stop_receiver: mio_channel::Receiver<()>, domain_id: u16) {
  let poll = Poll::new().unwrap();

  // adjust domain_id or participant_id if necessary to interoperability
  let domain_participant = DomainParticipant::new(domain_id);

  let mut pub_qos = QosPolicies::qos_none();
  pub_qos.reliability = Some(Reliability::BestEffort);
  // pub_qos.reliability = Some(Reliability::Reliable {
  //   max_blocking_time: Duration::from(StdDuration::from_millis(100)),
  // });
  pub_qos.history = Some(History::KeepLast { depth: 1 });
  pub_qos.ownership = Some(Ownership::Shared);
  pub_qos.durability = Some(Durability::Volatile);
  pub_qos.liveliness = Some(Liveliness {
    kind: LivelinessKind::Automatic,
    lease_duration: Duration::DURATION_INFINITE,
  });
  pub_qos.destination_order = Some(DestinationOrder::ByReceptionTimestamp);
  pub_qos.resource_limits = Some(ResourceLimits {
    max_instances: std::i32::MAX,
    max_samples: std::i32::MAX,
    max_samples_per_instance: std::i32::MAX,
  });
  pub_qos.deadline = Some(Deadline {
    period: Duration::DURATION_INFINITE,
  });
  pub_qos.latency_budget = Some(LatencyBudget {
    duration: Duration::DURATION_ZERO,
  });
  pub_qos.presentation = Some(Presentation {
    access_scope: PresentationAccessScope::Instance,
    coherent_access: false,
    ordered_access: false,
  });
  pub_qos.lifespan = Some(Lifespan {
    duration: Duration::DURATION_INFINITE,
  });

  // declare topics, subscriber, publisher, readers and writers
  let square_topic = domain_participant
    .create_topic("Square", TypeDesc::new(String::from("ShapeType")), &pub_qos)
    .unwrap();
  let triangle_topic = domain_participant
    .create_topic(
      "Triangle",
      TypeDesc::new(String::from("ShapeType")),
      &pub_qos,
    )
    .unwrap();

  let square_sub = domain_participant
    .create_subscriber(&QosPolicies::qos_none())
    .unwrap();

  // reader needs to be mutable if you want to read/take something from it
  let mut square_reader = square_sub
    .create_datareader::<Square, CDR_deserializer_adapter<Square>>(
      None,
      &square_topic,
      None,
      QosPolicies::qos_none(),
    )
    .unwrap();

  let square_pub = domain_participant.create_publisher(&pub_qos).unwrap();
  let mut square_writer = square_pub
    .create_datawriter::<Square, CDR_serializer_adapter<Square, LittleEndian>>(
      None,
      &triangle_topic,
      &pub_qos,
    )
    .unwrap();

  // register readers and possible timers
  poll
    .register(
      &stop_receiver,
      STOP_EVENT_LOOP_TOKEN,
      Ready::readable(),
      PollOpt::edge(),
    )
    .unwrap();
  poll
    .register(
      &square_reader,
      SQUARE_READER_TOKEN,
      Ready::readable(),
      PollOpt::edge(),
    )
    .unwrap();

  let stdout_org = std::io::stdout();
  let mut areader = termion::async_stdin().bytes();
  {
    let mut stdout = stdout_org.lock().into_raw_mode().unwrap();
    write!(
      stdout,
      "{}{} ",
      termion::clear::All,
      termion::cursor::Goto(1, 1)
    )
    .unwrap();
    stdout.flush().unwrap();
  }

  let mut input_timer = Timer::default();
  input_timer.set_timeout(StdDuration::from_millis(KEYBOARD_CHECK_TIMEOUT), ());
  poll
    .register(
      &input_timer,
      KEYBOARD_CHECK_TOKEN,
      Ready::readable(),
      PollOpt::edge(),
    )
    .unwrap();

  let mut row: u16 = 0;
  let mut square = Square::new(String::from("BLUE"), 0, 0, 30);
  loop {
    {
      if row > 60 {
        let mut stdout = stdout_org.lock().into_raw_mode().unwrap();
        row = 1;
        write!(
          stdout,
          "{}{}",
          termion::clear::All,
          termion::cursor::Goto(1, row)
        )
        .unwrap();
        stdout.flush().unwrap();
      }

      let mut events = Events::with_capacity(10);
      poll.poll(&mut events, None).unwrap();

      for event in events.iter() {
        let mut stdout = stdout_org.lock().into_raw_mode().unwrap();

        if event.token() == STOP_EVENT_LOOP_TOKEN {
          return;
        } else if event.token() == SQUARE_READER_TOKEN {
          let squares = fetch_squares(&mut square_reader);
          for square in squares.iter() {
            write!(stdout, "{}", termion::cursor::Goto(1, row)).unwrap();
            write!(stdout, "Item: {:?} received", square).unwrap();
            stdout.flush().unwrap();
            row += 1;
          }
        } else if event.token() == KEYBOARD_CHECK_TOKEN {
          let mut square_moved = false;
          let mut dispose_square = false;

          while let Some(c) = areader.next() {
            write!(stdout, "{}", termion::cursor::Goto(1, row)).unwrap();

            let c = match c {
              Ok(c) => c,
              _ => {
                continue;
              }
            };
            match c {
              113 => return,
              65 => {
                square.yadd(-1);
                square_moved = true;
              }
              66 => {
                square.yadd(1);
                square_moved = true;
              }
              67 => {
                square.xadd(1);
                square_moved = true;
              }
              68 => {
                square.xadd(-1);
                square_moved = true;
              }
              100 => {
                dispose_square = true;
              }
              _ => {
                continue;
              }
            };
          }
          if square_moved {
            match square_writer.write(square.clone(), None) {
              Ok(_) => (),
              Err(e) => println!("Failed to write new square. {:?}", e),
            };
          }

          if dispose_square {
            println!("Disposing square");
            match square_writer.dispose(square.get_key(), None) {
              Ok(_) => (),
              Err(e) => println!("Failed to dispose square. {:?}", e),
            }
          }

          input_timer.set_timeout(StdDuration::from_millis(KEYBOARD_CHECK_TIMEOUT), ());
        }
      }
    }
  }
}

fn fetch_squares(reader: &mut DataReader<Square, CDR_deserializer_adapter<Square>>) -> Vec<Square> {
  match reader.take(100, ReadCondition::any()) {
    Ok(ds) => ds
      .iter()
      .filter_map(|p| p.get_value())
      .map(|p| p.clone())
      .collect(),
    Err(_) => {
      println!("Failed to read squares");
      vec![]
    }
  }
}

fn stop_control(stop_sender: mio_channel::SyncSender<()>) {
  let running = Arc::new(AtomicBool::new(true));
  let r = running.clone();

  ctrlc::set_handler(move || {
    r.store(false, Ordering::SeqCst);
  })
  .expect("Error setting Ctrl-C handler");

  println!("Waiting for Ctrl-C...");
  while running.load(Ordering::SeqCst) {}

  match stop_sender.try_send(()) {
    Ok(_) => (),
    _ => println!("EventLoop is already finished."),
  };
  println!("Got it! Exiting...");
}